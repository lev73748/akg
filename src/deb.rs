// Формат .deb: ar, control.tar.*, data.tar.*; распаковка в .dpkg-new + md5 на лету.
// Безопасность: пути валидируются до записи (без "..", только внутри prefix,
// реальный путь родителя не должен выходить за prefix через симлинки).

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use md5::{Digest, Md5};
use rayon::prelude::*;

use crate::db::{Cfg, Control};
use crate::util::*;

struct ArMember {
    name: String,
    off: u64,
    size: u64,
}

fn ar_members(path: &Path) -> R<Vec<ArMember>> {
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let mut magic = [0u8; 8];
    f.read_exact(&mut magic).map_err(|e| format!("{}: {e}", path.display()))?;
    if &magic != b"!<arch>\n" {
        return Err(format!("{}: not a .deb (ar) archive", path.display()));
    }
    let mut pos = 8u64;
    let mut out = Vec::new();
    while pos + 60 <= len {
        let mut h = [0u8; 60];
        f.read_exact(&mut h).map_err(|e| format!("{}: {e}", path.display()))?;
        if &h[58..60] != &b"`\n"[..] {
            return Err(format!("{}: bad ar member header", path.display()));
        }
        let name = String::from_utf8_lossy(&h[0..16]).trim().trim_end_matches('/').to_string();
        let size: u64 = String::from_utf8_lossy(&h[48..58])
            .trim()
            .parse()
            .map_err(|_| format!("{}: bad ar member size", path.display()))?;
        let off = pos + 60;
        out.push(ArMember { name, off, size });
        pos = off + size + (size & 1);
        f.seek(SeekFrom::Start(pos)).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn open_member(path: &Path, m: &ArMember) -> R<Box<dyn Read>> {
    let mut f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.seek(SeekFrom::Start(m.off)).map_err(|e| e.to_string())?;
    let r = io::BufReader::with_capacity(256 * 1024, f.take(m.size));
    if m.name.ends_with(".xz") {
        Ok(Box::new(xz2::read::XzDecoder::new(r)))
    } else if m.name.ends_with(".gz") {
        Ok(Box::new(flate2::read::GzDecoder::new(r)))
    } else if m.name.ends_with(".tar") {
        Ok(Box::new(r))
    } else {
        Err(format!("unsupported compression in member '{}' (xz/gz/plain only)", m.name))
    }
}

pub struct Pkg {
    pub name: String,
    pub version: String,
    pub arch: String,
    pub path: PathBuf,
    pub ctrl: Control,
    pub files: HashMap<String, Vec<u8>>,
    pub conffiles: Vec<String>,
    pub replaces: HashSet<String>,
}

/// Имя пакета попадает в пути info/<name>.* и tmp.ci/<name>, поэтому строго:
/// первый символ буква/цифра, дальше буквы, цифры и + - . _
pub fn valid_pkg_name(n: &str) -> bool {
    let mut ch = n.chars();
    match ch.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    n.len() <= 128 && n.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.' | '_'))
}

pub fn prepare(path: &Path) -> R<Pkg> {
    let members = ar_members(path)?;
    let m = members
        .iter()
        .find(|m| m.name.starts_with("control.tar"))
        .ok_or_else(|| format!("{}: no control.tar member", path.display()))?;
    let rd = open_member(path, m)?;
    let mut ar = tar::Archive::new(rd);
    let mut files: HashMap<String, Vec<u8>> = HashMap::new();
    for e in ar.entries().map_err(|e| e.to_string())? {
        let mut e = e.map_err(|e| e.to_string())?;
        if !e.header().entry_type().is_file() {
            continue;
        }
        let name = e
            .path()
            .map_err(|x| x.to_string())?
            .to_string_lossy()
            .trim_start_matches("./")
            .to_string();
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            continue;
        }
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).map_err(|x| x.to_string())?;
        files.insert(name, buf);
    }
    let ctrl = match files.get("control") {
        Some(c) => Control::parse(&String::from_utf8_lossy(c)),
        None => return Err(format!("{}: no control file", path.display())),
    };
    let name = ctrl.get("Package").ok_or("control has no Package field")?.to_string();
    if !valid_pkg_name(&name) {
        return Err(format!("{}: unsafe package name '{}'", path.display(), name));
    }
    let version = ctrl.get("Version").unwrap_or("").to_string();
    let arch = ctrl.get("Architecture").unwrap_or("").to_string();
    let mut conffiles = Vec::new();
    if let Some(b) = files.get("conffiles") {
        for l in String::from_utf8_lossy(b).lines() {
            let l = l.trim();
            if l.starts_with('/') {
                conffiles.push(l.to_string());
            }
        }
    }
    let mut replaces = HashSet::new();
    if let Some(v) = ctrl.get("Replaces") {
        for part in v.split(',') {
            for alt in part.split('|') {
                let n = alt
                    .trim()
                    .split(|c: char| c == ' ' || c == '(')
                    .next()
                    .unwrap_or("")
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .trim();
                if !n.is_empty() {
                    replaces.insert(n.to_string());
                }
            }
        }
    }
    Ok(Pkg { name, version, arch, path: path.to_path_buf(), ctrl, files, conffiles, replaces })
}

// ------------------------------------------------------------ data

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Dir,
    File,
    Sym,
    Hard,
}

pub struct FileEnt {
    pub name: String,
    pub kind: Kind,
    pub md5: Option<String>,
    pub pending: bool,
}

pub struct Extracted {
    pub entries: Vec<FileEnt>,
    pub steals: Vec<(String, String)>,
    pub new_paths: Vec<PathBuf>,
}

/// Нормализует путь из архива в "/a/b". Любой компонент ".." (или NUL) — ошибка.
fn norm_name(raw: &str) -> R<String> {
    let mut parts: Vec<&str> = Vec::new();
    for c in raw.split('/') {
        match c {
            "" | "." => {}
            ".." => return Err(format!("unsafe path in package (..): {raw}")),
            c if c.contains('\0') => return Err("unsafe path in package (NUL)".to_string()),
            c => parts.push(c),
        }
    }
    if parts.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

fn under_prefix(cfg: &Cfg, name: &str) -> bool {
    let pre = cfg.prefix.trim_end_matches('/');
    name == pre || name.strip_prefix(pre).map_or(false, |r| r.starts_with('/'))
}

/// Разрешено: prefix и всё внутри него; родительские каталоги prefix — только как каталоги.
fn allowed(cfg: &Cfg, name: &str, is_dir: bool) -> bool {
    let pre = cfg.prefix.trim_end_matches('/');
    under_prefix(cfg, name) || (is_dir && pre.strip_prefix(name).map_or(false, |r| r.starts_with('/')))
}

fn canon_prefix(cfg: &Cfg) -> R<PathBuf> {
    let p = cfg.fs(&cfg.prefix);
    fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    fs::canonicalize(&p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Ближайший существующий предок пути (с раскрытием симлинков) должен лежать внутри prefix.
fn check_inside(canon_pre: &Path, dir: &Path) -> R<()> {
    let mut cur = dir;
    loop {
        match fs::canonicalize(cur) {
            Ok(c) => {
                return if c.starts_with(canon_pre) {
                    Ok(())
                } else {
                    Err(format!("refusing to write outside the install prefix: {}", dir.display()))
                };
            }
            Err(_) => match cur.parent() {
                Some(p) => cur = p,
                None => return Err(format!("cannot resolve path: {}", dir.display())),
            },
        }
    }
}

pub fn cleanup_new(ex: &Extracted) {
    for p in &ex.new_paths {
        let _ = fs::remove_file(p);
    }
}

pub fn load_owners(cfg: &Cfg) -> HashMap<String, Vec<String>> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = fs::read_dir(cfg.info()) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if let Some(p) = n.strip_suffix(".list") {
                files.push((p.to_string(), e.path()));
            }
        }
    }
    let parts: Vec<(String, Vec<String>)> = files
        .par_iter()
        .map(|(p, path)| {
            let b = fs::read(path).unwrap_or_default();
            let t = String::from_utf8_lossy(&b);
            let lines: Vec<String> = t
                .lines()
                .filter(|l| !l.is_empty() && *l != "/.")
                .map(|l| l.to_string())
                .collect();
            (p.clone(), lines)
        })
        .collect();
    let mut m: HashMap<String, Vec<String>> = HashMap::new();
    for (p, ls) in parts {
        for l in ls {
            m.entry(l).or_default().push(p.clone());
        }
    }
    m
}

pub fn extract_data(cfg: &Cfg, pkg: &Pkg, owners: &HashMap<String, Vec<String>>) -> R<Extracted> {
    let mut ex = Extracted { entries: Vec::new(), steals: Vec::new(), new_paths: Vec::new() };
    match extract_inner(cfg, pkg, owners, &mut ex) {
        Ok(()) => Ok(ex),
        Err(e) => {
            cleanup_new(&ex);
            Err(format!("{}: {e}", pkg.name))
        }
    }
}

fn extract_inner(
    cfg: &Cfg,
    pkg: &Pkg,
    owners: &HashMap<String, Vec<String>>,
    ex: &mut Extracted,
) -> R<()> {
    let canon_pre = canon_prefix(cfg)?;
    let members = ar_members(&pkg.path)?;
    let m = members
        .iter()
        .find(|m| m.name.starts_with("data.tar"))
        .ok_or_else(|| "no data.tar member".to_string())?;
    let rd = open_member(&pkg.path, m)?;
    let mut archive = tar::Archive::new(rd);
    let mut md5s: HashMap<String, String> = HashMap::new();
    let mut last_parent = PathBuf::new();
    let mut buf = vec![0u8; 256 * 1024];

    for e in archive.entries().map_err(|x| x.to_string())? {
        let mut e = e.map_err(|x| format!("corrupt data archive: {x}"))?;
        let raw = e.path().map_err(|x| x.to_string())?.to_string_lossy().into_owned();
        let name = norm_name(&raw)?;
        if name.is_empty() {
            continue;
        }
        let et = e.header().entry_type();
        if !allowed(cfg, &name, et.is_dir()) {
            return Err(format!("refusing to write outside the install prefix: {name}"));
        }
        let mode = e.header().mode().unwrap_or(0o644) & 0o7777;
        let mtime = e.header().mtime().unwrap_or(0);
        let fsp = cfg.fs(&name);

        if !et.is_dir() {
            if let Some(os) = owners.get(&name) {
                for o in os {
                    if *o == pkg.name {
                        continue;
                    }
                    if pkg.replaces.contains(o) || cfg.force_overwrite {
                        ex.steals.push((o.clone(), name.clone()));
                    } else {
                        return Err(format!(
                            "trying to overwrite '{}', which is also in package {} (no Replaces)",
                            name, o
                        ));
                    }
                }
            }
        }

        if et.is_dir() {
            if fs::symlink_metadata(&fsp).is_err() {
                if under_prefix(cfg, &name) {
                    check_inside(&canon_pre, &fsp)?;
                }
                if let Some(p) = fsp.parent() {
                    fs::create_dir_all(p).map_err(|x| format!("mkdir {}: {x}", p.display()))?;
                }
                match fs::create_dir(&fsp) {
                    Ok(()) => {
                        let _ = fs::set_permissions(&fsp, Permissions::from_mode((mode | 0o700) & 0o7777));
                    }
                    Err(x) if x.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(x) => return Err(format!("mkdir {}: {x}", fsp.display())),
                }
            }
            ex.entries.push(FileEnt { name, kind: Kind::Dir, md5: None, pending: false });
        } else if et.is_file() {
            let parent = fsp.parent().unwrap_or(Path::new("/")).to_path_buf();
            if parent != last_parent {
                check_inside(&canon_pre, &parent)?;
                fs::create_dir_all(&parent).map_err(|x| format!("mkdir {}: {x}", parent.display()))?;
                last_parent = parent;
            }
            let newp = with_suffix(&fsp, ".dpkg-new");
            let _ = fs::remove_file(&newp);
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&newp)
                .map_err(|x| format!("create {}: {x}", newp.display()))?;
            ex.new_paths.push(newp.clone());
            let mut h = Md5::new();
            loop {
                let n = e.read(&mut buf).map_err(|x| format!("read {name}: {x}"))?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
                out.write_all(&buf[..n]).map_err(|x| format!("write {}: {x}", newp.display()))?;
            }
            let _ = out.set_permissions(Permissions::from_mode((mode | 0o400) & 0o7777));
            drop(out);
            set_mtime(&newp, mtime);
            let digest = hex(h.finalize().as_slice());
            md5s.insert(name.clone(), digest.clone());
            ex.entries.push(FileEnt { name, kind: Kind::File, md5: Some(digest), pending: true });
        } else if et.is_symlink() {
            let link = e
                .link_name()
                .map_err(|x| x.to_string())?
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            if link.is_empty() || link.contains('\0') {
                return Err(format!("bad symlink target for {name}"));
            }
            if let Some(p) = fsp.parent() {
                if p != last_parent {
                    check_inside(&canon_pre, p)?;
                    fs::create_dir_all(p).map_err(|x| format!("mkdir {}: {x}", p.display()))?;
                    last_parent = p.to_path_buf();
                }
            }
            if let Ok(md) = fs::symlink_metadata(&fsp) {
                if md.is_dir() {
                    ex.entries.push(FileEnt { name, kind: Kind::Sym, md5: None, pending: false });
                    continue;
                }
            }
            let newp = with_suffix(&fsp, ".dpkg-new");
            let _ = fs::remove_file(&newp);
            symlink(&link, &newp).map_err(|x| format!("symlink {}: {x}", newp.display()))?;
            ex.new_paths.push(newp);
            ex.entries.push(FileEnt { name, kind: Kind::Sym, md5: None, pending: true });
        } else if et.is_hard_link() {
            let link = e
                .link_name()
                .map_err(|x| x.to_string())?
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let target = norm_name(&link)?;
            if target.is_empty() || !allowed(cfg, &target, false) {
                return Err(format!("unsafe hard link target for {name}: {link}"));
            }
            let tfs = cfg.fs(&target);
            if let Some(tp) = tfs.parent() {
                check_inside(&canon_pre, tp)?;
            }
            let tnew = with_suffix(&tfs, ".dpkg-new");
            let src = if tnew.exists() { tnew } else { tfs };
            if let Some(p) = fsp.parent() {
                if p != last_parent {
                    check_inside(&canon_pre, p)?;
                    fs::create_dir_all(p).map_err(|x| format!("mkdir {}: {x}", p.display()))?;
                    last_parent = p.to_path_buf();
                }
            }
            let newp = with_suffix(&fsp, ".dpkg-new");
            let _ = fs::remove_file(&newp);
            fs::hard_link(&src, &newp)
                .map_err(|x| format!("hardlink {} -> {}: {x}", newp.display(), src.display()))?;
            ex.new_paths.push(newp);
            let digest = md5s.get(&target).cloned();
            if let Some(d) = &digest {
                md5s.insert(name.clone(), d.clone());
            }
            ex.entries.push(FileEnt { name, kind: Kind::Hard, md5: digest, pending: true });
        } else {
            eprintln!("{} skipping special file {}", paint("33", "warning:"), name);
        }
    }
    Ok(())
}

// ------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    const PRE: &str = "/data/data/com.termux/files/usr";

    fn tmp(n: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("akg-deb-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn cfg_in(root: &Path) -> Cfg {
        Cfg {
            root: root.to_string_lossy().into_owned(),
            prefix: PRE.to_string(),
            jobs: 1,
            yes: true,
            dry_run: false,
            force_overwrite: false,
            no_scripts: true,
        }
    }

    // tar-запись с произвольным именем (Builder не даёт писать "..")
    fn raw_entry(name: &str, data: &[u8]) -> Vec<u8> {
        let mut h = tar::Header::new_old();
        let b = name.as_bytes();
        h.as_old_mut().name[..b.len()].copy_from_slice(b);
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        let mut out = h.as_bytes().to_vec();
        out.extend_from_slice(data);
        let pad = (512 - data.len() % 512) % 512;
        out.extend(std::iter::repeat(0u8).take(pad));
        out
    }

    fn make_deb(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let mut tarb: Vec<u8> = Vec::new();
        for (n, d) in entries {
            tarb.extend(raw_entry(n, d));
        }
        tarb.extend(std::iter::repeat(0u8).take(1024));
        let mut deb = b"!<arch>\n".to_vec();
        let hdr = format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", "data.tar", 0, 0, 0, "100644", tarb.len());
        deb.extend(hdr.as_bytes());
        deb.extend(&tarb);
        if tarb.len() % 2 == 1 {
            deb.push(b'\n');
        }
        let p = dir.join("t.deb");
        fs::write(&p, deb).unwrap();
        p
    }

    fn pkg_at(path: PathBuf) -> Pkg {
        Pkg {
            name: "t".into(),
            version: "1".into(),
            arch: "all".into(),
            path,
            ctrl: Control::parse("Package: t\n"),
            files: HashMap::new(),
            conffiles: Vec::new(),
            replaces: HashSet::new(),
        }
    }

    #[test]
    fn norm_name_rules() {
        assert_eq!(norm_name("./a/./b/").unwrap(), "/a/b");
        assert_eq!(norm_name("./").unwrap(), "");
        assert!(norm_name("../x").is_err());
        assert!(norm_name("a/../b").is_err());
        assert!(norm_name("/data/../../etc/passwd").is_err());
    }

    #[test]
    fn pkg_names() {
        assert!(valid_pkg_name("libc++"));
        assert!(valid_pkg_name("python-pip"));
        assert!(!valid_pkg_name("../../x"));
        assert!(!valid_pkg_name("a/b"));
        assert!(!valid_pkg_name(""));
        assert!(!valid_pkg_name(".hidden"));
    }

    #[test]
    fn traversal_entry_rejected() {
        let root = tmp("trav");
        let deb = make_deb(&root, &[("./data/data/com.termux/files/usr/../../../../../evil", b"x")]);
        let r = extract_data(&cfg_in(&root), &pkg_at(deb), &HashMap::new());
        assert!(r.is_err());
        assert!(!root.join("evil").exists());
    }

    #[test]
    fn outside_prefix_rejected() {
        let root = tmp("outside");
        let deb = make_deb(&root, &[("./etc/passwd", b"x")]);
        assert!(extract_data(&cfg_in(&root), &pkg_at(deb), &HashMap::new()).is_err());
        assert!(!root.join("etc/passwd.dpkg-new").exists());
    }

    #[test]
    fn symlinked_parent_escape_rejected() {
        let root = tmp("symesc");
        let outside = tmp("symesc-out");
        let pre = root.join(PRE.trim_start_matches('/'));
        fs::create_dir_all(&pre).unwrap();
        std::os::unix::fs::symlink(&outside, pre.join("x")).unwrap();
        let deb = make_deb(&root, &[("./data/data/com.termux/files/usr/x/pwn", b"x")]);
        assert!(extract_data(&cfg_in(&root), &pkg_at(deb), &HashMap::new()).is_err());
        assert!(!outside.join("pwn.dpkg-new").exists());
    }

    #[test]
    fn valid_entry_extracts() {
        let root = tmp("ok");
        let deb = make_deb(&root, &[("./data/data/com.termux/files/usr/bin/ok", b"hello")]);
        let ex = extract_data(&cfg_in(&root), &pkg_at(deb), &HashMap::new()).unwrap();
        assert_eq!(ex.entries.len(), 1);
        assert!(root.join("data/data/com.termux/files/usr/bin/ok.dpkg-new").exists());
    }
}
