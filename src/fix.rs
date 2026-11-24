// fix-broken: донастроить пакеты (unpacked / half-configured),
// переустановить недоустановленные (half-installed), поставить недостающие зависимости.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::db::*;
use crate::install::{self, run_script};
use crate::repo::{self, Entry, Index};
use crate::resolve::{self, Request};
use crate::util::*;

struct Found {
    configure: Vec<String>, // unpacked, half-configured
    reinstall: Vec<String>, // half-installed
}

fn scan(db: &StatusDb) -> Found {
    let mut f = Found {
        configure: Vec::new(),
        reinstall: Vec::new(),
    };
    for n in &db.order {
        match db.status_of(n).as_deref() {
            Some("unpacked") | Some("half-configured") => f.configure.push(n.clone()),
            Some("half-installed") => f.reinstall.push(n.clone()),
            _ => {}
        }
    }
    f
}

// (пакет, группа зависимостей текстом, имя, которое надо поставить)
fn missing_deps(inst: &HashMap<String, Entry>) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut names: Vec<&String> = inst.keys().collect();
    names.sort();
    for n in names {
        let e = &inst[n];
        for g in e.pre_depends.iter().chain(e.depends.iter()) {
            let ok = g.iter().any(|d| {
                inst.get(&d.name)
                    .map_or(false, |x| resolve::dep_ok(d, &x.version))
                    || inst
                        .values()
                        .any(|x| x.provides.iter().any(|(pn, _)| *pn == d.name))
            });
            if !ok {
                out.push((n.clone(), resolve::fmt_group(g), g[0].name.clone()));
            }
        }
    }
    out
}

fn build_requests(
    pre: &Path,
    reinstall: &[String],
    missing: &[(String, String, String)],
) -> R<Vec<Request>> {
    if repo::read_sources(pre).is_empty() {
        return Err("no repositories configured".into());
    }
    if repo::is_custom() || !repo::lists_dir(pre).exists() {
        repo::update(pre)?;
    }
    let idx = Index::load(pre, &arch())?;
    let mut reqs: Vec<Request> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for n in reinstall {
        if idx.best(n).is_none() {
            eprintln!(
                "{} {n}: not found in repositories, cannot reinstall",
                paint("33", "warning:")
            );
            continue;
        }
        if seen.insert(n.clone()) {
            reqs.push(Request {
                name: n.clone(),
                version: None,
                reinstall: true,
            });
        }
    }
    for (pkg, group, name) in missing {
        let known = idx.by_name.contains_key(name) || idx.providers.contains_key(name);
        if !known {
            eprintln!(
                "{} {pkg}: nothing in repositories provides {group}",
                paint("33", "warning:")
            );
            continue;
        }
        if seen.insert(name.clone()) {
            reqs.push(Request {
                name: name.clone(),
                version: None,
                reinstall: false,
            });
        }
    }
    Ok(reqs)
}

pub fn run(cfg: &Cfg, pre: &Path) -> R<i32> {
    // 1. что сломано (замок держим только на время чтения)
    let (found, inst) = {
        let _lock = lock_db(cfg)?;
        let mut db = StatusDb::load(&cfg.admin().join("status"))?;
        incorporate_updates(cfg, &mut db)?;
        (scan(&db), db.installed())
    };
    let missing = missing_deps(&inst);

    if found.configure.is_empty() && found.reinstall.is_empty() && missing.is_empty() {
        println!("No broken packages found.");
        return Ok(0);
    }

    for n in &found.reinstall {
        println!(
            "  {} half-installed (unpack was interrupted): will be reinstalled",
            paint("1;31", n)
        );
    }
    for n in &found.configure {
        println!(
            "  {} not fully configured: will be configured",
            paint("1;33", n)
        );
    }
    for (pkg, group, _) in &missing {
        println!(
            "  {} unmet dependency: {}",
            paint("1;33", pkg),
            group
        );
    }
    if cfg.dry_run {
        return Ok(0);
    }

    let mut code = 0;

    // 2. переустановка и недостающие зависимости (нужна сеть)
    if !found.reinstall.is_empty() || !missing.is_empty() {
        match build_requests(pre, &found.reinstall, &missing) {
            Ok(reqs) if !reqs.is_empty() => match install::run_install(cfg, reqs, Vec::new(), false) {
                Ok(c) => {
                    if c != 0 {
                        code = c;
                    }
                }
                Err(e) => {
                    eprintln!("{} {}", paint("31", "error:"), e);
                    code = 1;
                }
            },
            Ok(_) => {}
            Err(e) => {
                eprintln!("{} {}", paint("33", "warning:"), e);
                code = 1;
            }
        }
    }

    // 3. донастройка всего, что осталось unpacked / half-configured
    let _lock = lock_db(cfg)?;
    let mut db = StatusDb::load(&cfg.admin().join("status"))?;
    incorporate_updates(cfg, &mut db)?;
    let inst2 = db.installed();
    let left = scan(&db);

    let mut order = resolve::removal_order(&inst2, &left.configure);
    order.reverse(); // сначала то, от чего зависят
    let total = order.len();
    let mut failed: Vec<String> = Vec::new();

    for (i, name) in order.iter().enumerate() {
        let Some(st) = db.get(name).cloned() else { continue };
        let arch_s = field(&st, "Architecture").unwrap_or_default();
        println!("[{}/{}] configuring {}", i + 1, total, paint("1;32", name));
        db.set(name, set_status(&st, "install ok half-configured"));
        let script = cfg.info().join(format!("{name}.postinst"));
        match run_script(cfg, name, &arch_s, &script, "postinst", &["configure"]) {
            Ok(()) => db.set(name, set_status(&st, "install ok installed")),
            Err(e) => {
                eprintln!("{} {}", paint("31", "error:"), e);
                failed.push(name.clone());
            }
        }
    }

    if total > 0 {
        let sp = cfg.admin().join("status");
        if sp.exists() {
            let _ = fs::copy(&sp, cfg.admin().join("status-old"));
        }
        db.save(&sp)?;
        unsafe {
            libc::sync();
        }
    }

    if !failed.is_empty() {
        eprintln!(
            "{} could not configure: {}",
            paint("31", "akg:"),
            failed.join(", ")
        );
        code = 1;
    }
    for n in &left.reinstall {
        if db.status_of(n).as_deref() == Some("half-installed") {
            eprintln!("{} {n} is still half-installed", paint("33", "warning:"));
            code = 1;
        }
    }
    if code == 0 {
        println!("{} done", paint("1;32", "akg:"));
    }
    Ok(code)
}
