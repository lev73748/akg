// Установка: каждый пакет скачивается и сразу распаковывается в своей задаче.
// Локальные .deb ставятся без сети. После установки запускаются триггеры.
// Кэш .deb переиспользуется только после проверки sha256.
// Прогресс: см. progress.rs, тайминги этапов печатаются в конце.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Permissions};
use std::io::{self, BufRead, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::db::*;
use crate::deb::*;
use crate::excache;
use crate::net::{self, Job};
use crate::progress::{self, Renderer, Shared};
use crate::repo::{parse_dep_list, Entry, Index};
use crate::resolve::{self, Request};
use crate::triggers;
use crate::util::*;

pub static NO_TRIGGERS: AtomicBool = AtomicBool::new(false);
pub static DOWNLOAD_ONLY: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Timings {
    fetch: Duration,
    unpack: Duration,
    configure: Duration,
    triggers: Duration,
}

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

// предупреждение, не ломающее строку прогресса
fn pwarn(r: R<()>) {
    if let Err(e) = r {
        progress::emit(&format!("{} {}", paint("33", "warning:"), e));
    }
}

// читает вывод скрипта построчно и печатает над баром
fn pump<T: Read>(r: T) {
    let mut br = io::BufReader::new(r);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        match br.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => progress::emit(&String::from_utf8_lossy(&buf)),
        }
    }
}

pub fn run_script(cfg: &Cfg, pkg: &str, arch: &str, path: &Path, name: &str, args: &[&str]) -> R<()> {
    if !cfg.scripts_enabled() || !path.exists() {
        return Ok(());
    }
    let _ = fs::set_permissions(path, Permissions::from_mode(0o755));
    let mut cmd = Command::new(path);
    cmd.args(args)
        .current_dir("/")
        .env("DPKG_MAINTSCRIPT_PACKAGE", pkg)
        .env("DPKG_MAINTSCRIPT_ARCH", arch)
        .env("DPKG_MAINTSCRIPT_NAME", name)
        .env("DPKG_MAINTSCRIPT_DEBUG", "0")
        .env("DPKG_ADMINDIR", cfg.admin());
    let fail = |e: io::Error| format!("cannot run {name} of {pkg}: {e}");

    let status = if progress::bar_active() {
        // бар на экране: вывод скрипта пропускаем через emit()
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(fail)?;
        let finished = Arc::new(AtomicUsize::new(0));
        let mut want = 0usize;
        if let Some(o) = child.stdout.take() {
            want += 1;
            let f = finished.clone();
            thread::spawn(move || {
                pump(o);
                f.fetch_add(1, Ordering::Relaxed);
            });
        }
        if let Some(er) = child.stderr.take() {
            want += 1;
            let f = finished.clone();
            thread::spawn(move || {
                pump(er);
                f.fetch_add(1, Ordering::Relaxed);
            });
        }
        let st = child.wait().map_err(fail)?;
        // если скрипт оставил фоновый процесс с открытым выводом, долго не ждём
        let t = Instant::now();
        while finished.load(Ordering::Relaxed) < want && t.elapsed() < Duration::from_millis(500) {
            thread::sleep(Duration::from_millis(5));
        }
        st
    } else {
        cmd.status().map_err(fail)?
    };

    if status.success() {
        Ok(())
    } else {
        Err(format!("{name} script of {pkg} failed ({status})"))
    }
}

// ------------------------------------------------------------ checks

/// Архитектура пакета подходит устройству (или "all"). Пустая — не подходит.
fn arch_ok(pkg_arch: &str, device: &str) -> bool {
    pkg_arch == "all" || pkg_arch == device
}

fn sha256_file(p: &Path) -> io::Result<String> {
    let mut f = File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(h.finalize().as_slice()))
}

/// Файл из кэша годится только если совпали и размер, и sha256 из индекса.
fn cache_valid(p: &Path, size: u64, want: &str) -> bool {
    let Ok(md) = fs::metadata(p) else { return false };
    if !md.is_file() || (size > 0 && md.len() != size) {
        return false;
    }
    sha256_file(p).map_or(false, |h| h == want)
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
            pwarn(write_atomic(&lp, s.as_bytes()));
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
        pwarn(run_script(
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
                    progress::emit(&format!(
                        "  conffile {} kept (modified locally), new version: {}",
                        en.name,
                        dist.display()
                    ));
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
        pwarn(run_script(
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
    tm: &mut Timings,
) -> R<()> {
    write_journal(cfg, recs)?;
    let mut result: R<()> = Ok(());
    // все затронутые пути (путь, это каталог) — для файловых триггеров
    let mut touched: Vec<(String, bool)> = Vec::new();
    let total = recs.len();
    let t_unpack = Instant::now();

    for i in 0..total {
        let name = recs[i].pkg.name.clone();
        let old = db.get(&name).and_then(|s| old_info(s));
        recs[i].old_version = old.as_ref().map(|o| o.version.clone());
        progress::step("Unpack    ", i, total, &name);

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
    tm.unpack = t_unpack.elapsed();
    progress::clear_line();

    if result.is_ok() {
        let t_conf = Instant::now();
        let todo: Vec<usize> = (0..total).filter(|&i| recs[i].st == St::Unpacked).collect();
        let nt = todo.len();
        for (k, &i) in todo.iter().enumerate() {
            let name = recs[i].pkg.name.clone();
            progress::step("Configure ", k, nt, &name);
            if let Err(e) = configure_one(cfg, &mut recs[i]) {
                result = Err(format!("{name}: {e}"));
                break;
            }
        }
        tm.configure = t_conf.elapsed();
        progress::clear_line();
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
        let t = Instant::now();
        triggers::run(cfg, db, &configured, &touched);
        tm.triggers = t.elapsed();
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

// Какие пакеты уже лежат в кэше целиком (совпали размер и sha256). Проверяем параллельно.
fn scan_cache(entries: &[Entry], archives: &Path, jobs: usize) -> HashSet<String> {
    let found: std::sync::Mutex<HashSet<String>> = std::sync::Mutex::new(HashSet::new());
    let next = AtomicUsize::new(0);
    let workers = jobs.clamp(1, 8).min(entries.len().max(1));
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(e) = entries.get(i) else { break };
                let fname = e.filename.rsplit('/').next().unwrap_or("");
                if fname.is_empty() || fname == "." || fname == ".." {
                    continue;
                }
                let Some(want) = e.sha256.as_deref() else { continue };
                if cache_valid(&archives.join(fname), e.size, want) {
                    found.lock().unwrap_or_else(|x| x.into_inner()).insert(e.name.clone());
                }
            });
        }
    });
    found.into_inner().unwrap_or_else(|x| x.into_inner())
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
        if !arch_ok(&pkg.arch, &my_arch) {
            return Err(format!(
                "{}: architecture '{}' does not match this device ('{}')",
                pkg.name, pkg.arch, my_arch
            ));
        }
        if !seen.insert(pkg.name.clone()) {
            continue;
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

    // то, что уже скачано и проверено, не качаем и в загрузку не считаем
    let cached: Arc<HashSet<String>> = Arc::new(scan_cache(&entries, &archives, cfg.jobs));
    let n_fetch = entries.iter().filter(|e| !cached.contains(&e.name)).count();
    let total: u64 = entries.iter().filter(|e| !cached.contains(&e.name)).map(|e| e.size).sum();
    if total > 0 {
        println!("Need to download {}", fmt_bytes(total as f64));
    }
    if !cached.is_empty() {
        println!("{} package(s) already in cache", cached.len());
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
    let mut tm = Timings::default();

    // общий счётчик прогресса для бара загрузки/распаковки
    let local_sizes: Vec<u64> =
        local_pkgs.iter().map(|p| fs::metadata(&p.path).map(|m| m.len()).unwrap_or(0)).collect();
    let ex_total: u64 = total + local_sizes.iter().sum::<u64>();
    let sh = Shared::new(n_fetch, if dl_only { 0 } else { n + local_pkgs.len() }, total, ex_total);
    let mut rend = Renderer::start(sh.clone());

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
            let sh = sh.clone();
            let cached = cached.clone();

            let h = tokio::spawn(async move {
                let _permit = sem.acquire().await.expect("sem closed");

                let fname = e.filename.rsplit('/').next().unwrap_or("").to_string();
                if fname.is_empty() || fname == "." || fname == ".." {
                    return Err(format!("{}: bad filename in package index", e.name));
                }
                // без хеша в индексе пакет не ставим
                let Some(want) = e.sha256.clone() else {
                    return Err(format!("{}: no SHA256 in package index, refusing to install", e.name));
                };
                let dst = archives.join(&fname);

                // кэш уже проверен до старта (размер И sha256): такие пакеты не качаем
                if !cached.contains(&e.name) {
                    let _ = fs::remove_file(&dst);
                    let job = Job {
                        url: format!("{}/{}", e.uri.trim_end_matches('/'), e.filename),
                        dest: dst.clone(),
                        size: e.size,
                        sha256: Some(want.clone()),
                    };
                    for attempt in 0..3u64 {
                        match net::download_one(&job, sh.dl_bytes.clone()).await {
                            Ok(()) => break,
                            Err(err) if attempt == 2 => {
                                return Err(format!("{}: {err}", e.name));
                            }
                            Err(_) => {
                                tokio::time::sleep(std::time::Duration::from_millis(300 + attempt * 200)).await;
                            }
                        }
                    }
                    sh.dl_done.fetch_add(1, Ordering::Relaxed);
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
                let ex = excache::extract_cached(&cfg_local, &pkg, &owners, &want)
                    .map_err(|err| format!("{}: {err}", e.name))?;
                sh.ex_bytes.fetch_add(e.size, Ordering::Relaxed);
                sh.ex_done.fetch_add(1, Ordering::Relaxed);
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
        rend.finish();
        for (_, x) in &pkgs {
            cleanup_new(x);
        }
        return Err(e);
    }

    if dl_only {
        rend.finish();
        println!(
            "{} downloaded {} package(s) to {} in {}",
            paint("1;32", "akg:"),
            n_fetch,
            archives.display(),
            progress::fmt_dur(t0.elapsed())
        );
        return Ok(0);
    }

    // локальные .deb распаковываем здесь (сеть не нужна)
    for (i, pkg) in local_pkgs.into_iter().enumerate() {
        match extract_data(cfg, &pkg, &owners) {
            Ok(ex) => {
                sh.ex_bytes.fetch_add(local_sizes[i], Ordering::Relaxed);
                sh.ex_done.fetch_add(1, Ordering::Relaxed);
                pkgs.push((pkg, ex));
            }
            Err(e) => {
                rend.finish();
                for (_, x) in &pkgs {
                    cleanup_new(x);
                }
                return Err(e);
            }
        }
    }
    rend.finish();
    tm.fetch = t0.elapsed();

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

    let res = run_transaction(cfg, &mut recs, &mut db, &owners, &mut tm);

    // установленные пакеты больше не нужны в кэше распаковки
    for r in &recs {
        if r.st == St::Installed && r.pkg.path.starts_with(&archives) {
            let _ = fs::remove_file(excache::manifest_path(&r.pkg.path));
        }
    }

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
    pwarn(marks.save(cfg));

    res?;

    progress::clear_line();
    println!(
        "{} {} package(s) processed in {}",
        paint("1;32", "akg:"),
        recs.len(),
        progress::fmt_dur(t0.elapsed())
    );
    let mut parts = vec![
        format!("fetch {}", progress::fmt_dur(tm.fetch)),
        format!("unpack {}", progress::fmt_dur(tm.unpack)),
        format!("configure {}", progress::fmt_dur(tm.configure)),
    ];
    if tm.triggers > Duration::ZERO {
        parts.push(format!("triggers {}", progress::fmt_dur(tm.triggers)));
    }
    println!("     {}", parts.join(" · "));
    Ok(0)
}

// ------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_rules() {
        assert!(arch_ok("all", "aarch64"));
        assert!(arch_ok("aarch64", "aarch64"));
        assert!(!arch_ok("arm", "aarch64"));
        assert!(!arch_ok("x86_64", "aarch64"));
        assert!(!arch_ok("", "aarch64"));
    }

    #[test]
    fn cache_needs_matching_size_and_hash() {
        let p = std::env::temp_dir().join(format!("akg-cache-{}", std::process::id()));
        fs::write(&p, b"hello").unwrap();
        let good = hex(Sha256::digest(b"hello").as_slice());
        assert!(cache_valid(&p, 5, &good));
        assert!(cache_valid(&p, 0, &good));
        assert!(!cache_valid(&p, 5, &"0".repeat(64)));
        assert!(!cache_valid(&p, 6, &good));
        // подмена содержимого при том же размере
        fs::write(&p, b"HELLO").unwrap();
        assert!(!cache_valid(&p, 5, &good));
        let _ = fs::remove_file(&p);
        assert!(!cache_valid(&p, 5, &good));
    }
}
