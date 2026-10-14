// Кэш распаковки. После успешной распаковки пакета рядом с .deb лежит манифест
// <имя>.deb.extracted со списком файлов и их md5. Если установку прервали, при повторном
// запуске уже распакованные пакеты проверяются (sha256 .deb, наличие и md5 файлов .dpkg-new,
// владельцы файлов) и не распаковываются заново. Не подошло — просто распаковываем с нуля.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::db::Cfg;
use crate::deb::{extract_data, Extracted, FileEnt, Kind, Pkg};
use crate::util::{md5_file, with_suffix, write_atomic, R};

const MAGIC: &str = "akg-extract 1";

pub fn manifest_path(deb: &Path) -> PathBuf {
    with_suffix(deb, ".extracted")
}

/// Распаковать пакет или взять результат из кэша. sha256 — хеш .deb из индекса.
pub fn extract_cached(
    cfg: &Cfg,
    pkg: &Pkg,
    owners: &HashMap<String, Vec<String>>,
    sha256: &str,
) -> R<Extracted> {
    let mf = manifest_path(&pkg.path);
    if let Some(ex) = load(cfg, pkg, owners, sha256, &mf) {
        return Ok(ex);
    }
    // манифест устарел или файлы повреждены: убираем остатки и распаковываем заново
    drop_cache(&mf);
    let ex = extract_data(cfg, pkg, owners)?;
    save(cfg, pkg, sha256, &ex, &mf);
    Ok(ex)
}

/// Удалить манифест и все .dpkg-new, которые он перечисляет.
pub fn drop_cache(mf: &Path) {
    if let Ok(text) = fs::read_to_string(mf) {
        for l in text.lines() {
            if let Some(p) = l.strip_prefix("N ") {
                let _ = fs::remove_file(p);
            }
        }
    }
    let _ = fs::remove_file(mf);
}

fn kind_char(k: Kind) -> char {
    match k {
        Kind::Dir => 'd',
        Kind::File => 'f',
        Kind::Sym => 's',
        Kind::Hard => 'h',
    }
}

fn save(cfg: &Cfg, pkg: &Pkg, sha256: &str, ex: &Extracted, mf: &Path) {
    let mut s = String::new();
    s.push_str(&format!("{MAGIC}\n"));
    s.push_str(&format!("sha256 {sha256}\n"));
    s.push_str(&format!("pkg {} {}\n", pkg.name, pkg.version));
    s.push_str(&format!("root {}\n", cfg.root));
    s.push_str(&format!("prefix {}\n", cfg.prefix));
    for e in &ex.entries {
        if e.name.contains('\n') || e.name.contains('\r') {
            return;
        }
        s.push_str(&format!(
            "E {} {} {} {}\n",
            kind_char(e.kind),
            if e.pending { 1 } else { 0 },
            e.md5.as_deref().unwrap_or("-"),
            e.name
        ));
    }
    for p in &ex.new_paths {
        let Some(t) = p.to_str() else { return };
        if t.contains('\n') || t.contains('\r') {
            return;
        }
        s.push_str(&format!("N {t}\n"));
    }
    s.push_str("end\n");
    let _ = write_atomic(mf, s.as_bytes());
}

fn load(
    cfg: &Cfg,
    pkg: &Pkg,
    owners: &HashMap<String, Vec<String>>,
    sha256: &str,
    mf: &Path,
) -> Option<Extracted> {
    let text = fs::read_to_string(mf).ok()?;
    let mut lines = text.lines();

    let head = [
        MAGIC.to_string(),
        format!("sha256 {sha256}"),
        format!("pkg {} {}", pkg.name, pkg.version),
        format!("root {}", cfg.root),
        format!("prefix {}", cfg.prefix),
    ];
    for h in &head {
        if lines.next()? != h.as_str() {
            return None;
        }
    }

    let mut entries: Vec<FileEnt> = Vec::new();
    let mut new_paths: Vec<PathBuf> = Vec::new();
    let mut complete = false;
    for l in lines {
        if let Some(r) = l.strip_prefix("E ") {
            let mut p = r.splitn(4, ' ');
            let kind = match p.next()? {
                "d" => Kind::Dir,
                "f" => Kind::File,
                "s" => Kind::Sym,
                "h" => Kind::Hard,
                _ => return None,
            };
            let pending = p.next()? == "1";
            let md5 = match p.next()? {
                "-" => None,
                m => Some(m.to_string()),
            };
            let name = p.next()?.to_string();
            entries.push(FileEnt { name, kind, md5, pending });
        } else if let Some(r) = l.strip_prefix("N ") {
            new_paths.push(PathBuf::from(r));
        } else if l == "end" {
            complete = true;
        } else {
            return None;
        }
    }
    if !complete {
        return None;
    }

    // файлы на диске должны быть на месте и совпадать с записанным md5
    for p in &new_paths {
        if fs::symlink_metadata(p).is_err() {
            return None;
        }
    }
    for e in &entries {
        let fsp = cfg.fs(&e.name);
        match e.kind {
            Kind::Dir => {
                if fs::symlink_metadata(&fsp).is_err() {
                    return None;
                }
            }
            Kind::File => {
                let newp = with_suffix(&fsp, ".dpkg-new");
                match (&e.md5, md5_file(&newp)) {
                    (Some(want), Ok(got)) if *want == got => {}
                    _ => return None,
                }
            }
            Kind::Sym | Kind::Hard => {}
        }
    }

    // владельцы файлов могли поменяться с прошлого запуска: пересчитываем конфликты
    let mut steals: Vec<(String, String)> = Vec::new();
    for e in &entries {
        if e.kind == Kind::Dir {
            continue;
        }
        if let Some(os) = owners.get(&e.name) {
            for o in os {
                if *o == pkg.name {
                    continue;
                }
                if pkg.replaces.contains(o) || cfg.force_overwrite {
                    steals.push((o.clone(), e.name.clone()));
                } else {
                    return None;
                }
            }
        }
    }

    Some(Extracted { entries, steals, new_paths })
}
