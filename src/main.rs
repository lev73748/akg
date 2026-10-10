#![allow(dead_code)]

mod db;
mod deb;
mod install;
mod net;
mod remove;
mod repo;
mod resolve;
mod triggers;
mod util;
mod version;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use db::Cfg;
use util::*;

fn help() {
    println!(
        "{} {} - package manager for Termux

{}
  akg [options] <command> [args]
  akg -f <file.deb>

{}
  -y, --yes             assume yes
  -j, --jobs N          parallel downloads
  -f, --file PATH       install a local .deb (path or URL), can be repeated
  -r, --repo URL        use this repository instead of sources.list, can be repeated
                        e.g. mirror.yandex.ru/mirrors/termux/termux-main
                        or a direct link to a .deb file
      --suite NAME      repository suite (default: stable)
      --comp NAME       repository component (default: main)
      --no-verify       skip Release signature check
      --download-only   only download .deb files
      --dry-run         show the plan only
      --no-scripts      do not run maintainer scripts
      --no-triggers     do not run triggers
      --force-overwrite allow overwriting files of other packages
  -V, --version         print version
  -h, --help            this help

{}
  update                refresh package index
  install <pkgs|files>  install packages, .deb files or .deb URLs  [i, add]
  reinstall <pkgs>      reinstall                        [rei]
  upgrade               upgrade all packages             [up, upg]
  uninstall <pkgs>      remove + unused dependencies     [remove, rm]
  purge <pkgs>          remove with config files
  autoremove            remove unused dependencies
  search <query>        search                           [se]
  show <pkgs>           package info                     [sh, info]
  files <pkg>           files of installed package       [f]
  list-installed        installed packages
  clean                 delete cached .deb files

Test mode: AKG_ROOT=$HOME/fakeroot akg install -y --no-scripts <pkg>",
        paint("1;32", "akg"),
        env!("CARGO_PKG_VERSION"),
        paint("1;33", "Usage:"),
        paint("1;33", "Options:"),
        paint("1;33", "Commands:")
    );
}

fn main() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let code = match run() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} {}", paint("1;31", "akg:"), e);
            1
        }
    };
    std::process::exit(code);
}

// Локальный путь или ссылка на .deb -> путь к файлу на диске.
fn deb_ref_to_path(cfg: &Cfg, s: &str) -> R<PathBuf> {
    if s.starts_with("http://") || s.starts_with("https://") {
        let clean = s.split(|c: char| c == '?' || c == '#').next().unwrap_or(s);
        let name = clean.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("package.deb");
        let dir = cfg.archives();
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let dst = dir.join(name);
        println!("{} {}", paint("36", "downloading"), s);
        net::download_file(s, &dst)?;
        Ok(dst)
    } else {
        let p = PathBuf::from(s);
        if !p.is_file() {
            return Err(format!("{s}: no such file"));
        }
        Ok(fs::canonicalize(&p).unwrap_or(p))
    }
}

fn run() -> R<i32> {
    let mut cfg = Cfg {
        root: std::env::var("AKG_ROOT").unwrap_or_default().trim_end_matches('/').to_string(),
        prefix: std::env::var("PREFIX").unwrap_or_else(|_| DEFAULT_PREFIX.into()),
        jobs: default_jobs(),
        yes: false,
        dry_run: false,
        force_overwrite: false,
        no_scripts: false,
    };

    // --opt=value -> --opt value
    let mut argv: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        if a.starts_with("--") {
            if let Some((k, v)) = a.split_once('=') {
                argv.push(k.to_string());
                argv.push(v.to_string());
                continue;
            }
        }
        argv.push(a);
    }

    let mut rest: Vec<String> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let mut repos: Vec<String> = Vec::new();
    let mut suite = String::from("stable");
    let mut comp = String::from("main");
    let mut it = argv.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "-j" | "--jobs" => {
                cfg.jobs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .filter(|&n: &usize| n > 0)
                    .ok_or("--jobs needs a positive number")?;
            }
            "-f" | "--file" => files.push(it.next().ok_or("--file needs a path")?),
            "-r" | "--repo" => repos.push(it.next().ok_or("--repo needs a URL")?),
            "--suite" => suite = it.next().ok_or("--suite needs a name")?,
            "--comp" | "--component" => comp = it.next().ok_or("--comp needs a name")?,
            "--no-verify" => repo::set_no_verify(true),
            "--download-only" => install::DOWNLOAD_ONLY.store(true, Ordering::Relaxed),
            "--no-triggers" => install::NO_TRIGGERS.store(true, Ordering::Relaxed),
            "-y" | "--yes" => cfg.yes = true,
            "--dry-run" => cfg.dry_run = true,
            "--no-scripts" => cfg.no_scripts = true,
            "--force-overwrite" => cfg.force_overwrite = true,
            "-V" | "--version" => {
                println!("akg {}", env!("CARGO_PKG_VERSION"));
                return Ok(0);
            }
            "-h" | "--help" => {
                help();
                return Ok(0);
            }
            _ => rest.push(a),
        }
    }

    // --repo со ссылкой на .deb = установка одного файла
    let mut real_repos: Vec<String> = Vec::new();
    for r in repos {
        if r.ends_with(".deb") {
            files.push(r);
        } else {
            real_repos.push(r);
        }
    }
    if !real_repos.is_empty() {
        repo::set_custom(&real_repos, &suite, &comp)?;
    }

    let cmd = if rest.is_empty() {
        if files.is_empty() {
            help();
            return Ok(1);
        }
        "install".to_string()
    } else {
        rest.remove(0)
    };
    let args = rest;
    let pre = PathBuf::from(&cfg.prefix);

    match cmd.as_str() {
        "help" => {
            help();
            Ok(0)
        }
        "update" | "upd" => {
            repo::update(&pre)?;
            Ok(0)
        }
        "install" | "i" | "add" | "reinstall" | "rei" => {
            let re = cmd.starts_with("rei");
            let mut names: Vec<String> = Vec::new();
            let mut debs: Vec<String> = files.clone();
            for a in &args {
                if a.starts_with('-') {
                    continue;
                }
                if a.ends_with(".deb") {
                    debs.push(a.clone());
                } else {
                    names.push(a.clone());
                }
            }
            if names.is_empty() && debs.is_empty() {
                return Err(format!("{cmd}: specify packages or a .deb file (-f)"));
            }
            let mut locals: Vec<PathBuf> = Vec::new();
            for d in &debs {
                locals.push(deb_ref_to_path(&cfg, d)?);
            }
            if !names.is_empty() {
                if repo::read_sources(&pre).is_empty() {
                    return Err("no repositories configured".into());
                }
                if repo::is_custom() || !repo::lists_dir(&pre).exists() {
                    repo::update(&pre)?;
                }
            }
            let reqs: Vec<resolve::Request> = names
                .iter()
                .map(|a| match a.split_once('=') {
                    Some((n, v)) => resolve::Request {
                        name: n.to_string(),
                        version: Some(v.to_string()),
                        reinstall: re,
                    },
                    None => resolve::Request { name: a.clone(), version: None, reinstall: re },
                })
                .collect();
            install::run_install(&cfg, reqs, locals, false)
        }
        "upgrade" | "up" | "upg" | "full-upgrade" => {
            repo::update(&pre)?;
            install::run_install(&cfg, Vec::new(), Vec::new(), true)
        }
        "uninstall" | "remove" | "rm" | "un" | "del" => {
            if args.is_empty() {
                return Err("uninstall: specify packages".into());
            }
            remove::run_remove(&cfg, &args, false, true)
        }
        "purge" => {
            if args.is_empty() {
                return Err("purge: specify packages".into());
            }
            remove::run_remove(&cfg, &args, true, true)
        }
        "autoremove" => remove::run_remove(&cfg, &[], false, true),
        "search" | "se" => {
            if repo::is_custom() {
                repo::update(&pre)?;
            }
            let idx = repo::Index::load(&pre, &arch())?;
            let terms: Vec<String> = args.iter().map(|a| a.to_lowercase()).collect();
            if terms.is_empty() {
                return Err("search: query required".into());
            }
            let mut hits: Vec<(u8, &repo::Entry)> = Vec::new();
            for v in idx.by_name.values() {
                let Some(e) = v.first() else { continue };
                let name = e.name.to_lowercase();
                let desc = e.desc.to_lowercase();
                let mut worst = 0u8;
                let mut ok = true;
                for t in &terms {
                    let sc = if name == *t {
                        0
                    } else if name.starts_with(t.as_str()) {
                        1
                    } else if name.contains(t.as_str()) {
                        2
                    } else if desc.contains(t.as_str()) {
                        3
                    } else {
                        ok = false;
                        break;
                    };
                    worst = worst.max(sc);
                }
                if ok {
                    hits.push((worst, e));
                }
            }
            hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
            for (_, e) in &hits {
                println!("{}/stable {}\n  {}\n", paint("1;32", &e.name), e.vstr, e.desc);
            }
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        "show" | "sh" | "info" | "inf" => {
            if repo::is_custom() {
                repo::update(&pre)?;
            }
            let idx = repo::Index::load(&pre, &arch())?;
            for n in &args {
                match idx.best(n) {
                    Some(e) => {
                        println!(
                            "Package: {}\nVersion: {}\nSize: {}\nFilename: {}/{}",
                            e.name,
                            e.vstr,
                            fmt_bytes(e.size as f64),
                            e.uri,
                            e.filename
                        );
                        for g in &e.depends {
                            println!("Depends: {}", resolve::fmt_group(g));
                        }
                        println!("Description: {}\n", e.desc);
                    }
                    None => eprintln!("package '{n}' not found"),
                }
            }
            Ok(0)
        }
        "files" | "f" => {
            if args.is_empty() {
                return Err("files: specify a package".into());
            }
            let mut code = 0;
            for a in &args {
                match fs::read(cfg.info().join(format!("{a}.list"))) {
                    Ok(b) => {
                        for l in String::from_utf8_lossy(&b).lines() {
                            if l != "/." {
                                println!("{l}");
                            }
                        }
                    }
                    Err(_) => {
                        eprintln!("{} package '{a}' is not installed", paint("33", "akg:"));
                        code = 1;
                    }
                }
            }
            Ok(code)
        }
        "list-installed" | "list-i" => {
            let db = db::StatusDb::load(&cfg.admin().join("status"))?;
            let inst = db.installed();
            let mut names: Vec<&String> = inst.keys().collect();
            names.sort();
            let marks = db::Marks::load(&cfg).autos();
            for n in names {
                let e = &inst[n];
                let tag = if marks.contains(n) { "[installed,automatic]" } else { "[installed]" };
                println!("{}/now {} {} {}", paint("1;32", n), e.vstr, e.arch, tag);
            }
            Ok(0)
        }
        "clean" => {
            let mut n = 0;
            let mut bytes = 0u64;
            if let Ok(rd) = fs::read_dir(cfg.archives()) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().map_or(false, |x| x == "deb") {
                        bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                        if fs::remove_file(&p).is_ok() {
                            n += 1;
                        }
                    }
                }
            }
            println!("removed {n} file(s), {}", fmt_bytes(bytes as f64));
            Ok(0)
        }
        _ => Err(format!("unknown command '{cmd}' (run 'akg help')")),
    }
}
