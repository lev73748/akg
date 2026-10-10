// Установка: каждый пакет скачивается и сразу распаковывается в своей задаче.
// Локальные .deb ставятся без сети. После установки запускаются триггеры.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::Semaphore;

use crate::db::*;
use crate::deb::*;
use crate::net::{self, Job};
use crate::repo::{parse_dep_list, Entry, Index};
use crate::resolve::{self, Request};
use crate::triggers;
use crate::util::*;

pub static NO_TRIGGERS: AtomicBool = AtomicBool::new(false);
pub static DOWNLOAD_ONLY: AtomicBool = AtomicBool::new(false);

pub fn confirm(cfg: &Cfg) -> bool {
    if cfg.yes {
        return true;
    }
    eprint!("Continue? [Y/n] ");
    let mut l = String::new();
    if io::stdin().read_line(&mut l).is_err() {
        return false;
    }
    let l = l.trim().to_lowercase();
    !(l == "n" || l == "no")
}

pub fn run_script(cfg: &Cfg, pkg: &str, arch: &str, path: &Path, name: &str, args: &[&str]) -> R<()> {
    if !cfg.scripts_enabled() || !path.exists() {
        return Ok(());
    }
    let _ = fs::set_permissions(path, Permissions::from_mode(0o755));
    let status = Command::new(path)
        .args(args)
        .current_dir("/")
        .env("DPKG_MAINTSCRIPT_PACKAGE", pkg)
        .env("DPKG_MAINTSCRIPT_ARCH", arch)
        .env("DPKG_MAINTSCRIPT_NAME", name)
        .env("DPKG_MAINTSCRIPT_DEBUG", "0")
        .env("DPKG_ADMINDIR", cfg.admin())
        .status()
        .map_err(|e| format!("cannot run {name} of {pkg}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} script of {pkg} failed ({status})"))
    }
}

// ------------------------------------------------------------ transaction

#[derive(Clone, Copy, PartialEq)]
enum St {
    Pending,
    HalfInstalled,
    Unpacked,
    HalfConfigured,
    Installed,
}

struct Rec {
    pkg: Pkg,
    ex: Option<Extracted>,
    st: St,
    conf: Vec<(String, String)>,
    old_version: Option<String>,
}

fn rename_new(newp: &Path, fsp: &Path) -> R<()> {
    fs::rename(newp, fsp).map_err(|e| format!("cannot install {}: {e}", fsp.display()))
}

fn apply_steals(cfg: &Cfg, steals: &[(String, String)]) {
    let mut by: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (o, p) in steals {
        by.entry(o.as_str()).or_default().insert(p.as_str());
    }
    for (o, set) in by {
        let lp = cfg.info().join(format!("{o}.list"));
        if let Ok(b) = fs::read(&lp) {
            let t = String::from_utf8_lossy(&b);
            let keep: Vec<&str> = t.lines().filter(|l| !set.contains(l)).collect();
            let mut s = keep.join("\n");
            s.push('\n');
            warn(write_atomic(&lp, s.as_bytes()));
        }
    }
}

fn remove_old_files(
    cfg: &Cfg,
    pkg: &Pkg,
    ex: &Extracted,
    old: &OldInfo,
    owners: &HashMap<String, Vec<String>>,
) {
    let lp = cfg.info().join(format!("{}.list", pkg.name));
    let t = match fs::read(&lp) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => return,
    };
    let newset: HashSet<&str> = ex.entries.iter().map(|e| e.name.as_str()).collect();
    let lines: Vec<&str> = t.lines().collect();
    for l in lines.iter().rev() {
        let l = *l;
        if l.is_empty() || l == "/." || newset.contains(l) {
            continue;
        }
        if owners.get(l).map_or(false, |os| os.iter().any(|o| *o != pkg.name)) {
            continue;
        }
        if old.conf.contains_key(l) {
            continue;
        }
        let fsp = cfg.fs(l);
        match fs::symlink_metadata(&fsp) {
            Ok(md) if md.is_dir() => {
                let _ = fs::remove_dir(&fsp);
            }
            Ok(_) => {
                let _ = fs::remove_file(&fsp);
            }
            Err(_) => {}
        }
    }
}

fn write_info(cfg: &Cfg, pkg: &Pkg, tmpci: &Path, ex: &Extracted) -> R<()> {
    let info = cfg.info();
    const KNOWN: &[&str] = &[
        "conffiles", "preinst", "postinst", "prerm", "postrm", "triggers", "shlibs", "symbols",
        "config", "templates",
    ];
    for k in KNOWN {
        if !pkg.files.contains_key(*k) {
            let _ = fs::remove_file(info.join(format!("{}.{}", pkg.name, k)));
        }
    }
    for n in pkg.files.keys() {
        if n == "control" || n.is_empty() || n.contains('/') {
            continue;
        }
        let dst = info.join(format!("{}.{}", pkg.name, n));
        fs::rename(tmpci.join(n), &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    }
    let mut list = String::from("/.\n");
    for en in &ex.entries {
        list.push_str(&en.name);
        list.push('\n');
    }
    write_atomic(&info.join(format!("{}.list", pkg.name)), list.as_bytes())?;
    if !pkg.files.contains_key("md5sums") {
        let mut s = String::new();
        for en in &ex.entries {
            if let Some(h) = &en.md5 {
                s.push_str(&format!("{}  {}\n", h, &en.name[1..]));
            }
        }
        write_atomic(&info.join(format!("{}.md5sums", pkg.name)), s.as_bytes())?;
    }
    Ok(())
}

fn unpack_inner(
    cfg: &Cfg,
    pkg: &Pkg,
    ex: &Extracted,
    old: Option<&OldInfo>,
    owners: &HashMap<String, Vec<String>>,
    st: &mut St,
    conf: &mut Vec<(String, String)>,
) -> R<()> {
    let info = cfg.info();
    let tmpci = cfg.admin().join("tmp.ci").join(&pkg.name);
    let _ = fs::remove_dir_all(&tmpci);
    fs::create_dir_all(&tmpci).map_err(|e| format!("{}: {e}", tmpci.display()))?;
    for (n, data) in &pkg.files {
        if n == "control" || n.is_empty() || n.contains('/') {
            continue;
        }
        let p = tmpci.join(n);
        fs::write(&p, data).map_err(|e| format!("{}: {e}", p.display()))?;
        let script = matches!(n.as_str(), "preinst" | "postinst" | "prerm" | "postrm" | "config");
        let _ = fs::set_permissions(&p, Permissions::from_mode(if script { 0o755 } else { 0o644 }));
    }

    if old.is_some() {
        warn(run_script(
            cfg,
            &pkg.name,
            &pkg.arch,
            &info.join(format!("{}.prerm", pkg.name)),
            "prerm",
            &["upgrade", &pkg.version],
        ));
    }
    let pre = tmpci.join("preinst");
    match old {
        Some(o) => run_script(cfg, &pkg.name, &pkg.arch, &pre, "preinst", &["upgrade", &o.version])?,
        None => run_script(cfg, &pkg.name, &pkg.arch, &pre, "preinst", &["install"])?,
    }
    *st = St::HalfInstalled;

    let conf_set: HashSet<&str> = pkg.conffiles.iter().map(|s| s.as_str()).collect();
    let empty: HashMap<String, String> = HashMap::new();
    let old_conf = old.map(|o| &o.conf).unwrap_or(&empty);
    let mut conf_new: Vec<(String, String)> = Vec::new();

    for en in &ex.entries {
        if !en.pending {
            continue;
        }
        let fsp = cfg.fs(&en.name);
        let newp = with_suffix(&fsp, ".dpkg-new");
        if en.kind == Kind::File && conf_set.contains(en.name.as_str()) {
            let newh = en.md5.clone().unwrap_or_default();
            if fs::symlink_metadata(&fsp).is_err() {
                rename_new(&newp, &fsp)?;
            } else {
                let cur = md5_file(&fsp).unwrap_or_default();
                if cur == newh {
                    let _ = fs::remove_file(&newp);
                } else if old_conf.get(&en.name).map(|s| s.as_str()) == Some(cur.as_str()) {
                    rename_new(&newp, &fsp)?;
                } else {
                    let dist = with_suffix(&fsp, ".dpkg-dist");
                    let _ = fs::rename(&newp, &dist);
                    eprintln!("  conffile {} kept (modified locally), new version: {}", en.name, dist.display());
                }
            }
            conf_new.push((en.name.clone(), newh));
        } else {
            rename_new(&newp, &fsp)?;
        }
    }

    apply_steals(cfg, &ex.steals);
    if let Some(o) = old {
        remove_old_files(cfg, pkg, ex, o, owners);
        warn(run_script(
            cfg,
            &pkg.name,
            &pkg.arch,
            &info.join(format!("{}.postrm", pkg.name)),
            "postrm",
            &["upgrade", &pkg.version],
        ));
    }
    write_info(cfg, pkg, &tmpci, ex)?;
    let _ = fs::remove_dir_all(&tmpci);
    *conf = conf_new;
    *st = St::Unpacked;
    Ok(())
}

fn configure_one(cfg: &Cfg, rec: &mut Rec) -> R<()> {
    rec.st = St::HalfConfigured;
    let mut args: Vec<&str> = vec!["configure"];
    if let Some(v) = &rec.old_version {
        args.push(v.as_str());
    }
    run_script(
        cfg,
        &rec.pkg.name,
        &rec.pkg.arch,
        &cfg.info().join(format!("{}.postinst", rec.pkg.name)),
        "postinst",
        &args,
    )?;
    rec.st = St::Installed;
    Ok(())
}

fn write_journal(cfg: &Cfg, recs: &[Rec]) -> R<()> {
    let dir = cfg.admin().join("updates");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut body = String::new();
    for r in recs {
        body.push_str(&build_stanza(&r.pkg.ctrl, "install reinstreq half-installed", &[]));
        body.push('\n');
    }
    let tmp = dir.join("tmp.i");
    let dst = dir.join("0000");
    let mut f = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);
    fs::rename(&tmp, &dst).map_err(|e| format!("{}: {e}", dst.display()))
}

fn commit(cfg: &Cfg, db: &mut StatusDb, recs: &[Rec]) -> R<()> {
    for r in recs {
        let status = match r.st {
            St::Pending => continue,
            St::HalfInstalled => "install reinstreq half-installed",
            St::Unpacked => "install ok unpacked",
            St::HalfConfigured => "install ok half-configured",
            St::Installed => "install ok installed",
        };
        db.set(&r.pkg.name, build_stanza(&r.pkg.ctrl, status, &r.conf));
    }
    let sp = cfg.admin().join("status");
    if sp.exists() {
        let _ = fs::copy(&sp, cfg.admin().join("status-old"));
    }
    db.save(&sp)?;
    for n in update_files(cfg) {
        let _ = fs::remove_file(cfg.admin().join("updates").join(n));
    }
    unsafe {
        libc::sync();
    }
    Ok(())
}

fn run_transaction(
    cfg: &Cfg,
    recs: &mut Vec<Rec>,
    db: &mut StatusDb,
    owners: &HashMap<String, Vec<String>>,
) -> R<()> {
    write_journal(cfg, recs)?;
    let mut result: R<()> = Ok(());
    // все затронутые пути (путь, это каталог) — для файловых триггеров
    let mut touched: Vec<(String, bool)> = Vec::new();

    for i in 0..recs.len() {
        let name = recs[i].pkg.name.clone();
        let old = db.get(&name).and_then(|s| old_info(s));
        recs[i].old_version = old.as_ref().map(|o| o.version.clone());
        println!("  {} {} ({})", paint("36", "unpacking"), name, recs[i].pkg.version);

        let exv = match recs[i].ex.take() {
            Some(x) => x,
            None => {
                result = Err(format!("{name}: internal error, not extracted"));
                break;
            }
        };
        for en in &exv.entries {
            touched.push((en.name.clone(), en.kind == Kind::Dir));
        }
        let r = {
            let rec = &mut recs[i];
            unpack_inner(cfg, &rec.pkg, &exv, old.as_ref(), owners, &mut rec.st, &mut rec.conf)
        };
        if let Err(e) = r {
            cleanup_new(&exv);
            result = Err(format!("{name}: {e}"));
            break;
        }
    }

    if result.is_ok() {
        for rec in recs.iter_mut() {
            if rec.st == St::Unpacked {
                println!("  {} {}", paint("36", "configuring"), rec.pkg.name);
                if let Err(e) = configure_one(cfg, rec) {
                    result = Err(format!("{}: {e}", rec.pkg.name));
                    break;
                }
            }
        }
    }

    for rec in recs.iter_mut() {
        if let Some(ex) = rec.ex.take() {
            cleanup_new(&ex);
        }
    }
    let c = commit(cfg, db, recs);

    if result.is_ok() && c.is_ok() && cfg.scripts_enabled() && !NO_TRIGGERS.load(Ordering::Relaxed) {
        let configured: HashSet<String> = recs
            .iter()
            .filter(|r| r.st == St::Installed)
            .map(|r| r.pkg.name.clone())
            .collect();
        triggers::run(cfg, db, &configured, &touched);
    }
    result.and(c)
}

// ------------------------------------------------------------ deps check for local .deb

fn missing_deps(inst: &HashMap<String, Entry>, planned: &HashSet<String>, ctrl: &Control) -> Vec<String> {
    let mut miss = Vec::new();
    for key in ["Pre-Depends", "Depends"] {
        let Some(v) = ctrl.get(key) else { continue };
        for group in parse_dep_list(v) {
            let ok = group.iter().any(|d| {
                planned.contains(&d.name)
                    || inst.get(&d.name).map_or(false, |e| resolve::dep_ok(d, &e.version))
                    || inst.values().any(|e| e.provides.iter().any(|(pn, _)| *pn == d.name))
            });
            if !ok {
                miss.push(resolve::fmt_group(&group));
            }
        }
    }
    miss
}

// ------------------------------------------------------------ driver

pub fn run_install(cfg: &Cfg, mut reqs: Vec<Request>, locals: Vec<PathBuf>, upgrade_all: bool) -> R<i32> {
    let need_index = upgrade_all || !reqs.is_empty();
    let idx: Option<Index> =
        if need_index { Some(Index::load(Path::new(&cfg.prefix), &arch())?) } else { None };
    let _lock = lock_db(cfg)?;
    let mut db = StatusDb::load(&cfg.admin().join("status"))?;
    incorporate_updates(cfg, &mut db)?;
    let inst = db.installed();

    // локальные .deb
    let mut local_pkgs: Vec<Pkg> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let my_arch = arch();
    for p in &locals {
        let pkg = prepare(p)?;
        if !seen.insert(pkg.name.clone()) {
            continue;
        }
        if !pkg.arch.is_empty() && pkg.arch != "all" && pkg.arch != my_arch {
            eprintln!(
                "{} {}: architecture '{}' does not match this device ('{}')",
                paint("33", "warning:"),
                pkg.name,
                pkg.arch,
                my_arch
            );
        }
        local_pkgs.push(pkg);
    }
    let local_names: HashSet<String> = local_pkgs.iter().map(|p| p.name.clone()).collect();

    let mut explicit: HashSet<String> =
        if upgrade_all { HashSet::new() } else { reqs.iter().map(|r| r.name.clone()).collect() };
    explicit.extend(local_names.iter().cloned());

    // пакеты из репозитория
    let mut entries: Vec<Entry> = Vec::new();
    if let Some(idx) = &idx {
        if upgrade_all {
            reqs = resolve::upgrade_requests(idx, &inst);
        }
        if !reqs.is_empty() {
            let plan = resolve::plan_install(idx, &inst, &reqs)?;
            if !plan.remove.is_empty() {
                return Err(format!(
                    "these installed packages conflict and must be removed first: {} (akg uninstall ...)",
                    plan.remove.join(", ")
                ));
            }
            entries = plan.install;
        }
    }
    entries.retain(|e| !local_names.contains(&e.name));

    if entries.is_empty() && local_pkgs.is_empty() {
        println!("Nothing to do.");
        return Ok(0);
    }

    let n = entries.len();
    let archives = cfg.archives();
    fs::create_dir_all(&archives).map_err(|e| format!("{}: {e}", archives.display()))?;

    println!("{} package(s):", n + local_pkgs.len());
    for e in &entries {
        let tag = if inst.contains_key(&e.name) { "upgrade" } else { "new" };
        println!("  {} {} [{}]", paint("1;32", &e.name), e.vstr, tag);
    }
    for p in &local_pkgs {
        let tag = if inst.contains_key(&p.name) { "upgrade" } else { "new" };
        println!("  {} {} [{}, local]", paint("1;32", &p.name), p.version, tag);
    }

    let planned: HashSet<String> =
        entries.iter().map(|e| e.name.clone()).chain(local_names.iter().cloned()).collect();
    for p in &local_pkgs {
        let miss = missing_deps(&inst, &planned, &p.ctrl);
        if !miss.is_empty() {
            eprintln!(
                "{} {} has unmet dependencies: {}",
                paint("33", "warning:"),
                p.name,
                miss.join(", ")
            );
        }
    }

    let total: u64 = entries.iter().map(|e| e.size).sum();
    if total > 0 {
        println!("Need to download {}", fmt_bytes(total as f64));
    }
    if cfg.dry_run {
        return Ok(0);
    }
    if !confirm(cfg) {
        println!("Aborted.");
        return Ok(1);
    }

    let dl_only = DOWNLOAD_ONLY.load(Ordering::Relaxed);
    let owners = Arc::new(load_owners(cfg));
    let t0 = Instant::now();
    let bytes_counter = Arc::new(AtomicU64::new(0));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    let results: Vec<Result<Option<(Pkg, Extracted)>, String>> = rt.block_on(async {
        let sem = Arc::new(Semaphore::new(cfg.jobs.max(2)));
        let mut handles = Vec::with_capacity(n);

        for e in entries {
            let archives = archives.clone();
            let owners = owners.clone();
            let cfg_root = cfg.root.clone();
            let cfg_prefix = cfg.prefix.clone();
            let force = cfg.force_overwrite;
            let dl = dl_only;
            let sem = sem.clone();
            let bytes_counter = bytes_counter.clone();

            let h = tokio::spawn(async move {
                let _permit = sem.acquire().await.expect("sem closed");

                let fname = e.filename.rsplit('/').next().unwrap_or(&e.filename).to_string();
                let dst = archives.join(&fname);

                let have = fs::metadata(&dst)
                    .map(|m| e.size > 0 && m.len() == e.size)
                    .unwrap_or(false);

                if !have {
                    let job = Job {
                        url: format!("{}/{}", e.uri.trim_end_matches('/'), e.filename),
                        dest: dst.clone(),
                        size: e.size,
                        sha256: e.sha256.clone(),
                    };
                    for attempt in 0..3u64 {
                        match net::download_one(&job, bytes_counter.clone()).await {
                            Ok(()) => break,
                            Err(err) if attempt == 2 => {
                                return Err(format!("{}: {err}", e.name));
                            }
                            Err(_) => {
                                tokio::time::sleep(std::time::Duration::from_millis(300 + attempt * 200)).await;
                            }
                        }
                    }
                }

                if dl {
                    return Ok(None);
                }

                let cfg_local = Cfg {
                    root: cfg_root,
                    prefix: cfg_prefix,
                    jobs: 1,
                    yes: true,
                    dry_run: false,
                    force_overwrite: force,
                    no_scripts: true,
                };
                let pkg = prepare(&dst).map_err(|err| format!("{}: {err}", e.name))?;
                let ex = extract_data(&cfg_local, &pkg, &owners)
                    .map_err(|err| format!("{}: {err}", e.name))?;
                eprintln!("\r\x1b[K  {} {}", paint("36", "ready"), pkg.name);
                Ok(Some((pkg, ex)))
            });
            handles.push(h);
        }

        let mut out = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(r) => out.push(r),
                Err(e) => out.push(Err(format!("task join: {e}"))),
            }
        }
        out
    });

    let mut pkgs: Vec<(Pkg, Extracted)> = Vec::new();
    let mut first_err: Option<String> = None;
    for r in results {
        match r {
            Ok(Some(px)) => pkgs.push(px),
            Ok(None) => {}
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if let Some(e) = first_err {
        for (_, x) in &pkgs {
            cleanup_new(x);
        }
        return Err(e);
    }

    if dl_only {
        println!("{} downloaded to {}", paint("1;32", "akg:"), archives.display());
        return Ok(0);
    }

    // локальные .deb распаковываем здесь (сеть не нужна)
    for pkg in local_pkgs {
        println!("  {} {}", paint("36", "reading"), pkg.name);
        match extract_data(cfg, &pkg, &owners) {
            Ok(ex) => pkgs.push((pkg, ex)),
            Err(e) => {
                for (_, x) in &pkgs {
                    cleanup_new(x);
                }
                return Err(e);
            }
        }
    }

    println!("  all packages ready in {:.1}s", t0.elapsed().as_secs_f64());

    let mut recs: Vec<Rec> = pkgs
        .into_iter()
        .map(|(pkg, ex)| Rec {
            pkg,
            ex: Some(ex),
            st: St::Pending,
            conf: Vec::new(),
            old_version: None,
        })
        .collect();

    let res = run_transaction(cfg, &mut recs, &mut db, &owners);

    let mut marks = Marks::load(cfg);
    for r in &recs {
        if r.st != St::Installed {
            continue;
        }
        let nm = &r.pkg.name;
        if explicit.contains(nm) {
            marks.set(nm, &r.pkg.arch, false);
        } else if !inst.contains_key(nm) {
            marks.set(nm, &r.pkg.arch, true);
        }
    }
    warn(marks.save(cfg));

    res?;
    println!("{} {} package(s) processed", paint("1;32", "akg:"), recs.len());
    Ok(0)
}
