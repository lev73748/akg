// src/remove.rs
// Удаление по логике dpkg remove.c: prerm, файлы в обратном порядке, postrm,
// config-files, purge. Плюс autoremove по меткам auto-installed.
// Прогресс и тайминги — как в install.rs.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::{Duration, Instant};

use crate::db::*;
use crate::deb::load_owners;
use crate::install::{confirm, run_script};
use crate::progress;
use crate::resolve;
use crate::util::*;

const BACKUP_EXTS: &[&str] = &[
    "~",
    ".bak",
    "%",
    ".dpkg-tmp",
    ".dpkg-new",
    ".dpkg-old",
    ".dpkg-dist",
    ".dpkg-bak",
];

fn is_numbered(suf: &str) -> bool {
    suf.len() > 2
        && suf.starts_with('~')
        && suf.ends_with('~')
        && suf[1..suf.len() - 1].bytes().all(|b| b.is_ascii_digit())
}

fn used_by_others(
    owners: &HashMap<String, Vec<String>>,
    path: &str,
    me: &str,
    removed: &HashSet<String>,
) -> bool {
    owners
        .get(path)
        .map_or(false, |v| v.iter().any(|o| o != me && !removed.contains(o)))
}

fn purge_one(
    cfg: &Cfg,
    name: &str,
    arch: &str,
    stanza: &str,
    owners: &HashMap<String, Vec<String>>,
    removed: &HashSet<String>,
) -> R<()> {
    for (cf, _) in conffiles_of(stanza) {
        let p = cfg.fs(&cf);
        let _ = fs::remove_file(&p);
        if let (Some(dir), Some(base)) = (p.parent(), p.file_name().and_then(|b| b.to_str())) {
            if let Ok(rd) = fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let suffix_backup = n
                        .strip_prefix(base)
                        .map_or(false, |suf| BACKUP_EXTS.contains(&suf) || is_numbered(suf));
                    let hash_backup = n.len() >= 2
                        && n.starts_with('#')
                        && n.ends_with('#')
                        && &n[1..n.len() - 1] == base;
                    if suffix_backup || hash_backup {
                        let _ = fs::remove_file(e.path());
                    }
                }
            }
        }
    }
    run_script(
        cfg,
        name,
        arch,
        &cfg.info().join(format!("{name}.postrm")),
        "postrm",
        &["purge"],
    )?;

    let lp = cfg.info().join(format!("{name}.list"));
    if let Ok(b) = fs::read(&lp) {
        let text = String::from_utf8_lossy(&b).into_owned();
        for l in text.lines().rev() {
            if l.is_empty() || l == "/." || used_by_others(owners, l, name, removed) {
                continue;
            }
            let _ = fs::remove_dir(cfg.fs(l));
        }
    }
    let pre = format!("{name}.");
    if let Ok(rd) = fs::read_dir(cfg.info()) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(suf) = n.strip_prefix(pre.as_str()) {
                if !suf.contains('.') {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
    Ok(())
}

fn remove_one(
    cfg: &Cfg,
    name: &str,
    arch: &str,
    stanza: &str,
    purge: bool,
    owners: &HashMap<String, Vec<String>>,
    removed: &HashSet<String>,
    db: &mut StatusDb,
) -> R<()> {
    let info = cfg.info();
    let status_path = cfg.admin().join("status");

    run_script(
        cfg,
        name,
        arch,
        &info.join(format!("{name}.prerm")),
        "prerm",
        &["remove"],
    )?;
    db.set(
        name,
        set_status(stanza, "deinstall reinstreq half-installed"),
    );
    db.save(&status_path)?;

    let conf: HashSet<String> = conffiles_of(stanza).into_iter().map(|(n, _)| n).collect();
    let lp = info.join(format!("{name}.list"));
    let text = fs::read(&lp)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let mut leftover: Vec<&str> = Vec::new();

    for l in lines.iter().rev() {
        let l = *l;
        if l.is_empty() || l == "/." || conf.contains(l) {
            if !l.is_empty() {
                leftover.push(l);
            }
            continue;
        }
        let fsp = cfg.fs(l);
        let md = match fs::symlink_metadata(&fsp) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if md.is_dir() {
            if used_by_others(owners, l, name, removed) {
                continue;
            }
            if fs::remove_dir(&fsp).is_err() {
                leftover.push(l);
            }
        } else {
            let _ = fs::remove_file(with_suffix(&fsp, ".dpkg-tmp"));
            let _ = fs::remove_file(with_suffix(&fsp, ".dpkg-new"));
            if let Err(e) = fs::remove_file(&fsp) {
                progress::emit(&format!(
                    "{} cannot remove {}: {e}",
                    paint("33", "warning:"),
                    fsp.display()
                ));
            }
        }
    }
    leftover.reverse();
    let mut s = leftover.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    write_atomic(&lp, s.as_bytes())?;

    run_script(
        cfg,
        name,
        arch,
        &info.join(format!("{name}.postrm")),
        "postrm",
        &["remove"],
    )?;

    let has_postrm = info.join(format!("{name}.postrm")).exists();
    let pre = format!("{name}.");
    if let Ok(rd) = fs::read_dir(&info) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(suf) = n.strip_prefix(pre.as_str()) {
                if !suf.contains('.') && suf != "list" && suf != "postrm" {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    if purge || (!has_postrm && conf.is_empty()) {
        purge_one(cfg, name, arch, stanza, owners, removed)?;
        db.remove(name);
    } else {
        db.set(name, set_status(stanza, "deinstall ok config-files"));
    }
    db.save(&status_path)
}

pub fn run_remove(cfg: &Cfg, names: &[String], purge: bool, autorem: bool) -> R<i32> {
    let _lock = lock_db(cfg)?;
    let mut db = StatusDb::load(&cfg.admin().join("status"))?;
    incorporate_updates(cfg, &mut db)?;
    let inst = db.installed();
    let mut marks = Marks::load(cfg);

    let mut targets: Vec<String> = Vec::new();
    let mut cfg_only: Vec<String> = Vec::new();
    for n in names {
        if inst.contains_key(n) {
            targets.push(n.clone());
        } else if purge && db.status_of(n).as_deref() == Some("config-files") {
            cfg_only.push(n.clone());
        } else {
            eprintln!("{} {} is not installed", paint("33", "akg:"), n);
        }
    }
    if targets.is_empty() && cfg_only.is_empty() && !autorem {
        return Ok(1);
    }

    let mut set: Vec<String> = if targets.is_empty() {
        Vec::new()
    } else {
        resolve::removal_closure(&inst, &targets)
    };
    if autorem {
        let rest: HashMap<String, _> = inst
            .iter()
            .filter(|(n, _)| !set.contains(n))
            .map(|(n, e)| (n.clone(), e.clone()))
            .collect();
        let extra = resolve::autoremove_set(&rest, &marks.autos());
        set.extend(extra);
        set.sort();
        set.dedup();
    }
    for n in &set {
        if inst.get(n).map_or(false, |e| e.essential) {
            return Err(format!(
                "{n} is an essential package, refusing to remove it"
            ));
        }
    }
    if set.is_empty() && cfg_only.is_empty() {
        println!("Nothing to remove.");
        return Ok(0);
    }

    println!("To {}:", if purge { "purge" } else { "remove" });
    for n in set.iter().chain(cfg_only.iter()) {
        let why = if targets.contains(n) || cfg_only.contains(n) {
            ""
        } else {
            "  (unused dependency/dependent)"
        };
        println!("  {}{}", paint("1;31", n), why);
    }
    if cfg.dry_run {
        return Ok(0);
    }
    if !confirm(cfg) {
        println!("Aborted.");
        return Ok(1);
    }

    let owners = load_owners(cfg);
    let mut removed: HashSet<String> = HashSet::new();
    let order = resolve::removal_order(&inst, &set);
    let mut result: R<()> = Ok(());

    let t0 = Instant::now();
    let mut t_remove = Duration::ZERO;
    let mut t_purge = Duration::ZERO;

    // фаза Remove (remove_one; внутри может быть purge, если --purge)
    if !order.is_empty() {
        let t = Instant::now();
        let total = order.len();
        let label = if purge { "Purge     " } else { "Remove    " };
        for (i, n) in order.iter().enumerate() {
            let stanza = match db.get(n) {
                Some(s) => s.clone(),
                None => continue,
            };
            let arch = inst.get(n).map(|e| e.arch.clone()).unwrap_or_default();
            progress::step(label, i, total, n);
            match remove_one(cfg, n, &arch, &stanza, purge, &owners, &removed, &mut db) {
                Ok(()) => {
                    removed.insert(n.clone());
                    marks.remove(n);
                }
                Err(e) => {
                    result = Err(format!("{n}: {e}"));
                    break;
                }
            }
        }
        t_remove = t.elapsed();
        progress::clear_line();
    }

    // фаза Purge для config-files only
    if result.is_ok() && !cfg_only.is_empty() {
        let t = Instant::now();
        let total = cfg_only.len();
        for (i, n) in cfg_only.iter().enumerate() {
            let stanza = match db.get(n) {
                Some(s) => s.clone(),
                None => continue,
            };
            let arch = field(&stanza, "Architecture").unwrap_or_default();
            progress::step("Purge     ", i, total, n);
            if let Err(e) = purge_one(cfg, n, &arch, &stanza, &owners, &removed) {
                result = Err(format!("{n}: {e}"));
                break;
            }
            db.remove(n);
            marks.remove(n);
            removed.insert(n.clone());
        }
        t_purge = t.elapsed();
        progress::clear_line();
        warn(db.save(&cfg.admin().join("status")));
    } else if result.is_ok() {
        warn(db.save(&cfg.admin().join("status")));
    }

    warn(marks.save(cfg));
    result?;

    progress::clear_line();
    println!(
        "{} {} package(s) processed in {}",
        paint("1;32", "akg:"),
        removed.len(),
        progress::fmt_dur(t0.elapsed())
    );
    let mut parts = Vec::new();
    if t_remove > Duration::ZERO {
        parts.push(format!(
            "{} {}",
            if purge { "purge" } else { "remove" },
            progress::fmt_dur(t_remove)
        ));
    }
    if t_purge > Duration::ZERO {
        parts.push(format!("purge {}", progress::fmt_dur(t_purge)));
    }
    if !parts.is_empty() {
        println!("     {}", parts.join(" · "));
    }
