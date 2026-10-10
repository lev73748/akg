// Репозитории: sources (deb822 + legacy), InRelease + проверка подписи через gpgv,
// проверка sha256-цепочки Release -> Packages -> .deb, разбор индекса, Index.
// Свой репозиторий через --repo (set_custom), отдельный каталог индексов.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use rayon::prelude::*;
use sha2::{Digest, Sha256};

use crate::net;
use crate::util::*;
use crate::version::{Op, Version};

// ------------------------------------------------------------ dependencies

#[derive(Clone, Debug)]
pub struct Dep {
    pub name: String,
    pub rel: Option<(Op, Version)>,
}

impl fmt::Display for Dep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.rel {
            Some((op, v)) => write!(f, "{} ({} {})", self.name, op, v),
            None => write!(f, "{}", self.name),
        }
    }
}

fn parse_dep(s: &str) -> Option<Dep> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (head, rest) = match s.find('(') {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let name = head.trim().split(':').next().unwrap_or("").trim();
    if name.is_empty() {
        return None;
    }
    let rel = rest.and_then(|r| {
        let r = r.split(')').next()?.trim();
        let (op, ver) = if let Some(v) = r.strip_prefix(">=") {
            (Op::Ge, v)
        } else if let Some(v) = r.strip_prefix("<=") {
            (Op::Le, v)
        } else if let Some(v) = r.strip_prefix(">>") {
            (Op::Gt, v)
        } else if let Some(v) = r.strip_prefix("<<") {
            (Op::Lt, v)
        } else if let Some(v) = r.strip_prefix('=') {
            (Op::Eq, v)
        } else if let Some(v) = r.strip_prefix('>') {
            (Op::Ge, v)
        } else if let Some(v) = r.strip_prefix('<') {
            (Op::Le, v)
        } else {
            return None;
        };
        Some((op, Version::parse(ver.trim())))
    });
    Some(Dep { name: name.to_string(), rel })
}

pub fn parse_dep_list(s: &str) -> Vec<Vec<Dep>> {
    s.split(',')
        .filter_map(|g| {
            let alts: Vec<Dep> = g.split('|').filter_map(parse_dep).collect();
            if alts.is_empty() {
                None
            } else {
                Some(alts)
            }
        })
        .collect()
}

fn flat(s: &str) -> Vec<Dep> {
    parse_dep_list(s).into_iter().flatten().collect()
}

// ------------------------------------------------------------ package entry

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub version: Version,
    pub vstr: String,
    pub arch: String,
    pub depends: Vec<Vec<Dep>>,
    pub pre_depends: Vec<Vec<Dep>>,
    pub conflicts: Vec<Dep>,
    pub breaks: Vec<Dep>,
    pub replaces: Vec<Dep>,
    pub provides: Vec<(String, Option<Version>)>,
    pub filename: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub essential: bool,
    pub priority: String,
    pub desc: String,
    pub uri: String,
    pub status: String,
}

pub fn parse_entry(st: &str) -> Option<Entry> {
    let mut m: HashMap<String, String> = HashMap::new();
    let mut cur: Option<String> = None;
    for line in st.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(k) = &cur {
                if let Some(v) = m.get_mut(k) {
                    v.push('\n');
                    v.push_str(line.trim());
                }
            }
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            m.insert(k.clone(), v.trim().to_string());
            cur = Some(k);
        }
    }
    let name = m.get("package")?.clone();
    let vstr = m.get("version").cloned().unwrap_or_default();
    let get = |k: &str| m.get(k).map(|s| s.as_str()).unwrap_or("");
    let provides = flat(get("provides"))
        .into_iter()
        .map(|d| (d.name, d.rel.map(|(_, v)| v)))
        .collect();
    Some(Entry {
        version: Version::parse(&vstr),
        arch: get("architecture").to_string(),
        depends: parse_dep_list(get("depends")),
        pre_depends: parse_dep_list(get("pre-depends")),
        conflicts: flat(get("conflicts")),
        breaks: flat(get("breaks")),
        replaces: flat(get("replaces")),
        provides,
        filename: get("filename").to_string(),
        size: get("size").parse().unwrap_or(0),
        sha256: m.get("sha256").map(|s| s.to_lowercase()),
        essential: get("essential") == "yes",
        priority: get("priority").to_string(),
        desc: get("description").lines().next().unwrap_or("").to_string(),
        uri: String::new(),
        status: get("status").split_whitespace().nth(2).unwrap_or("").to_string(),
        vstr,
        name,
    })
}

pub fn parse_entries(text: &str, uri: &str, arch: &str) -> Vec<Entry> {
    text.split("\n\n")
        .filter(|s| !s.trim().is_empty())
        .filter_map(parse_entry)
        .filter(|e| e.arch == "all" || e.arch == arch || e.arch.is_empty())
        .map(|mut e| {
            e.uri = uri.to_string();
            e
        })
        .collect()
}

pub fn load_installed(status_path: &Path) -> HashMap<String, Entry> {
    let text = fs::read(status_path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let mut m = HashMap::new();
    for st in text.split("\n\n") {
        if let Some(e) = parse_entry(st) {
            if matches!(
                e.status.as_str(),
                "installed" | "unpacked" | "half-configured" | "half-installed"
                    | "triggers-pending" | "triggers-awaited"
            ) {
                m.insert(e.name.clone(), e);
            }
        }
    }
    m
}

// ------------------------------------------------------------ custom repos (--repo)

#[derive(Clone)]
pub struct Source {
    pub uri: String,
    pub suite: String,
    pub comps: Vec<String>,
    pub signed_by: Option<String>,
}

static CUSTOM: OnceLock<Vec<Source>> = OnceLock::new();
static NO_VERIFY: AtomicBool = AtomicBool::new(false);

pub fn set_no_verify(v: bool) {
    NO_VERIFY.store(v, Ordering::Relaxed);
}

pub fn is_custom() -> bool {
    CUSTOM.get().is_some()
}

// "mirror.yandex.ru/mirrors/termux/termux-main" -> "https://mirror.yandex.ru/mirrors/termux/termux-main"
pub fn normalize_repo(s: &str) -> String {
    let t = s.trim().trim_end_matches('/');
    if t.starts_with("http://") || t.starts_with("https://") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

pub fn set_custom(repos: &[String], suite: &str, comp: &str) -> R<()> {
    let mut v = Vec::new();
    for r in repos {
        if r.trim().is_empty() {
            return Err("empty repository url".into());
        }
        v.push(Source {
            uri: normalize_repo(r),
            suite: suite.to_string(),
            comps: vec![comp.to_string()],
            signed_by: None,
        });
    }
    CUSTOM.set(v).map_err(|_| "repositories already set".to_string())
}

// ------------------------------------------------------------ index

pub struct Index {
    pub by_name: HashMap<String, Vec<Entry>>,
    pub providers: HashMap<String, Vec<(String, Option<Version>)>>,
}

pub fn lists_dir(prefix: &Path) -> PathBuf {
    if is_custom() {
        prefix.join("var/lib/akg/lists-custom")
    } else {
        prefix.join("var/lib/akg/lists")
    }
}

impl Index {
    pub fn load(prefix: &Path, arch: &str) -> R<Index> {
        let dir = lists_dir(prefix);
        let only: Vec<String> = CUSTOM
            .get()
            .map(|c| c.iter().map(|s| format!("{}_", san(&s.uri))).collect())
            .unwrap_or_default();
        let mut files: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let ok = p.file_name().and_then(|n| n.to_str()).map_or(false, |n| {
                    n.ends_with("_Packages")
                        && (only.is_empty() || only.iter().any(|o| n.starts_with(o.as_str())))
                });
                if ok {
                    files.push(p);
                }
            }
        }
        if files.is_empty() {
            return Err("package index is empty, run 'akg update' first".into());
        }
        let parts: Vec<Vec<Entry>> = files
            .par_iter()
            .map(|p| {
                let text = fs::read(p)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let meta = fs::read_to_string(with_suffix(p, ".meta")).unwrap_or_default();
                let mut lines = meta.lines();
                let _hash = lines.next();
                let uri = lines.next().unwrap_or("").to_string();
                parse_entries(&text, &uri, arch)
            })
            .collect();

        let mut by_name: HashMap<String, Vec<Entry>> = HashMap::new();
        let mut providers: HashMap<String, Vec<(String, Option<Version>)>> = HashMap::new();
        for e in parts.into_iter().flatten() {
            for (pn, pv) in &e.provides {
                providers.entry(pn.clone()).or_default().push((e.name.clone(), pv.clone()));
            }
            by_name.entry(e.name.clone()).or_default().push(e);
        }
        for v in by_name.values_mut() {
            v.sort_by(|a, b| b.version.cmp(&a.version));
        }
        for v in providers.values_mut() {
            v.sort_by(|a, b| a.0.cmp(&b.0));
            v.dedup_by(|a, b| a.0 == b.0);
        }
        Ok(Index { by_name, providers })
    }

    pub fn best(&self, name: &str) -> Option<&Entry> {
        self.by_name.get(name).and_then(|v| v.first())
    }

    pub fn find(&self, name: &str, ver: &str) -> Option<&Entry> {
        let want = Version::parse(ver);
        self.by_name.get(name)?.iter().find(|e| e.version == want)
    }
}

// ------------------------------------------------------------ sources

fn parse_deb822(text: &str, out: &mut Vec<Source>) {
    for block in text.split("\n\n") {
        let mut m: HashMap<String, String> = HashMap::new();
        for line in block.lines() {
            if line.starts_with('#') || line.starts_with(' ') {
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                m.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        if !m.get("types").map_or(false, |t| t.split_whitespace().any(|x| x == "deb")) {
            continue;
        }
        if m.get("enabled").map_or(false, |e| e.eq_ignore_ascii_case("no")) {
            continue;
        }
        let uris = m.get("uris").cloned().unwrap_or_default();
        let suites = m.get("suites").cloned().unwrap_or_default();
        let comps = m.get("components").cloned().unwrap_or_default();
        for u in uris.split_whitespace() {
            for su in suites.split_whitespace() {
                out.push(Source {
                    uri: u.trim_end_matches('/').to_string(),
                    suite: su.to_string(),
                    comps: comps.split_whitespace().map(String::from).collect(),
                    signed_by: m.get("signed-by").cloned(),
                });
            }
        }
    }
}

fn parse_legacy(text: &str, out: &mut Vec<Source>) {
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut toks: Vec<&str> = line.split_whitespace().collect();
        if toks.first() != Some(&"deb") {
            continue;
        }
        toks.remove(0);
        let mut signed_by = None;
        if toks.first().map_or(false, |t| t.starts_with('[')) {
            let mut opts = String::new();
            while !toks.is_empty() {
                let t = toks.remove(0);
                opts.push_str(t);
                opts.push(' ');
                if t.ends_with(']') {
                    break;
                }
            }
            for o in opts.trim_matches(|c: char| c == '[' || c == ']' || c == ' ').split(' ') {
                if let Some(v) = o.strip_prefix("signed-by=") {
                    signed_by = Some(v.to_string());
                }
            }
        }
        if toks.len() < 3 {
            continue;
        }
        out.push(Source {
            uri: toks[0].trim_end_matches('/').to_string(),
            suite: toks[1].to_string(),
            comps: toks[2..].iter().map(|s| s.to_string()).collect(),
            signed_by,
        });
    }
}

pub fn read_sources(prefix: &Path) -> Vec<Source> {
    if let Some(c) = CUSTOM.get() {
        return c.clone();
    }
    let mut out = Vec::new();
    let etc = prefix.join("etc/apt");
    let mut files = vec![etc.join("sources.list")];
    if let Ok(rd) = fs::read_dir(etc.join("sources.list.d")) {
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        v.sort();
        files.extend(v);
    }
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else { continue };
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext == "sources" {
            parse_deb822(&text, &mut out);
        } else if ext == "list" {
            parse_legacy(&text, &mut out);
        }
    }
    out
}

// ------------------------------------------------------------ signatures

static WARNED: AtomicBool = AtomicBool::new(false);

fn warn_once(msg: &str) {
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!("{} {}", paint("33", "warning:"), msg);
    }
}

fn gpgv_ok() -> bool {
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        Command::new("gpgv")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn keyrings(prefix: &Path, signed_by: &Option<String>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(s) = signed_by {
        for p in s.split_whitespace() {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                v.push(pb);
            }
        }
    }
    if v.is_empty() {
        if let Ok(rd) = fs::read_dir(prefix.join("etc/apt/trusted.gpg.d")) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().map_or(false, |x| x == "gpg") {
                    v.push(p);
                }
            }
        }
        let t = prefix.join("etc/apt/trusted.gpg");
        if t.is_file() {
            v.push(t);
        }
    }
    v.sort();
    v
}

fn san(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

fn strip_clearsign(text: &str) -> String {
    if !text.contains("BEGIN PGP SIGNED MESSAGE") {
        return text.to_string();
    }
    let mut out = String::new();
    let mut inside = false;
    let mut hdr = false;
    for line in text.lines() {
        if line.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
            inside = true;
            hdr = true;
            continue;
        }
        if !inside {
            continue;
        }
        if hdr {
            if line.trim().is_empty() {
                hdr = false;
            }
            continue;
        }
        if line.starts_with("-----BEGIN PGP SIGNATURE-----") {
            break;
        }
        out.push_str(line.strip_prefix("- ").unwrap_or(line));
        out.push('\n');
    }
    out
}

fn run_gpgv(kr: &[PathBuf], files: &[&Path], base: &str) -> R<()> {
    let mut c = Command::new("gpgv");
    c.arg("--quiet");
    for k in kr {
        c.arg("--keyring").arg(k);
    }
    for f in files {
        c.arg(f);
    }
    let out = c.output().map_err(|e| format!("gpgv: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "signature verification failed for {base}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn verify_inrelease(prefix: &Path, s: &Source, base: &str, data: &[u8]) -> R<String> {
    let text = String::from_utf8_lossy(data).into_owned();
    if NO_VERIFY.load(Ordering::Relaxed) {
        return Ok(strip_clearsign(&text));
    }
    let kr = keyrings(prefix, &s.signed_by);
    if !gpgv_ok() || kr.is_empty() {
        warn_once("gpgv or keyrings not found: repository signatures are NOT verified (pkg install gpgv)");
        return Ok(strip_clearsign(&text));
    }
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let tmp = dir.join(format!("InRelease.{}", san(base)));
    fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let r = run_gpgv(&kr, &[tmp.as_path()], base);
    let _ = fs::remove_file(&tmp);
    r?;
    Ok(strip_clearsign(&text))
}

fn verify_detached(prefix: &Path, s: &Source, base: &str, rel: &[u8], sig: Option<Vec<u8>>) -> R<String> {
    let text = String::from_utf8_lossy(rel).into_owned();
    if NO_VERIFY.load(Ordering::Relaxed) {
        return Ok(text);
    }
    let kr = keyrings(prefix, &s.signed_by);
    match sig {
        Some(sig) if gpgv_ok() && !kr.is_empty() => {
            let dir = state_dir();
            let _ = fs::create_dir_all(&dir);
            let tr = dir.join(format!("Release.{}", san(base)));
            let ts = dir.join(format!("Release.gpg.{}", san(base)));
            fs::write(&tr, rel).map_err(|e| e.to_string())?;
            fs::write(&ts, &sig).map_err(|e| e.to_string())?;
            let r = run_gpgv(&kr, &[ts.as_path(), tr.as_path()], base);
            let _ = fs::remove_file(&tr);
            let _ = fs::remove_file(&ts);
            r?;
        }
        _ => warn_once("gpgv, keyrings or signature missing: repository signatures are NOT verified"),
    }
    Ok(text)
}

fn fetch_release(prefix: &Path, s: &Source, base: &str) -> R<String> {
    match net::fetch_bytes(&format!("{base}/dists/{}/InRelease", s.suite)) {
        Ok(data) => verify_inrelease(prefix, s, base, &data),
        Err(_) => {
            let rel = net::fetch_bytes(&format!("{base}/dists/{}/Release", s.suite))?;
            let sig = net::fetch_bytes(&format!("{base}/dists/{}/Release.gpg", s.suite)).ok();
            verify_detached(prefix, s, base, &rel, sig)
        }
    }
}

fn parse_release(text: &str) -> HashMap<String, (String, u64)> {
    let mut m = HashMap::new();
    let mut in_sha = false;
    for line in text.lines() {
        if line.starts_with("SHA256:") {
            in_sha = true;
            continue;
        }
        if !line.starts_with(' ') {
            in_sha = false;
            continue;
        }
        if in_sha {
            let mut p = line.split_whitespace();
            if let (Some(h), Some(sz), Some(path)) = (p.next(), p.next(), p.next()) {
                if let Ok(sz) = sz.parse::<u64>() {
                    m.insert(path.to_string(), (h.to_lowercase(), sz));
                }
            }
        }
    }
    m
}

fn decompress_index(name: &str, data: &[u8]) -> R<Vec<u8>> {
    let mut out = Vec::new();
    if name.ends_with(".xz") {
        xz2::read::XzDecoder::new(data).read_to_end(&mut out).map_err(|e| format!("xz: {e}"))?;
    } else if name.ends_with(".gz") {
        flate2::read::GzDecoder::new(data).read_to_end(&mut out).map_err(|e| format!("gz: {e}"))?;
    } else {
        out.extend_from_slice(data);
    }
    Ok(out)
}

fn update_source(prefix: &Path, s: &Source, arch: &str, dir: &Path) -> R<usize> {
    let base = s.uri.trim_end_matches('/').to_string();
    let rel_text = fetch_release(prefix, s, &base)?;
    let files = parse_release(&rel_text);
    let mut changed = 0;

    for comp in &s.comps {
        let stem = format!("{comp}/binary-{arch}/Packages");
        let pick = ["xz", "gz", ""]
            .iter()
            .map(|e| if e.is_empty() { stem.clone() } else { format!("{stem}.{e}") })
            .find(|p| files.contains_key(p));
        let Some(rel) = pick else {
            eprintln!("{} {base}: no index for {comp}/{arch}", paint("33", "warning:"));
            continue;
        };
        let (hash, size) = files[&rel].clone();
        let name = format!("{}_{}_{}_Packages", san(&base), san(&s.suite), san(comp));
        let list = dir.join(&name);
        let meta = with_suffix(&list, ".meta");
        if let Ok(m) = fs::read_to_string(&meta) {
            if m.lines().next() == Some(hash.as_str()) && list.exists() {
                continue;
            }
        }
        let data = net::fetch_bytes(&format!("{base}/dists/{}/{rel}", s.suite))?;
        if data.len() as u64 != size {
            return Err(format!("{rel}: size mismatch"));
        }
        let d = Sha256::digest(&data);
        if hex(d.as_slice()) != hash {
            return Err(format!("{rel}: sha256 mismatch (does not match signed Release)"));
        }
        let text = decompress_index(&rel, &data)?;
        write_atomic(&list, &text)?;
        write_atomic(&meta, format!("{hash}\n{base}\n").as_bytes())?;
        changed += 1;
    }
    Ok(changed)
}

pub fn update(prefix: &Path) -> R<()> {
    let srcs = read_sources(prefix);
    if srcs.is_empty() {
        return Err("no repositories configured in etc/apt".into());
    }
    let arch = arch();
    let dir = lists_dir(prefix);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    eprintln!("{} {} repositories, arch {}", paint("36", "akg: updating"), srcs.len(), arch);

    let res: Vec<(String, R<usize>)> = srcs
        .par_iter()
        .map(|s| (s.uri.clone(), update_source(prefix, s, &arch, &dir)))
        .collect();
    let mut errs = 0;
    for (u, r) in &res {
        match r {
            Ok(n) => eprintln!("  {} {} ({} updated)", paint("32", "ok"), u, n),
            Err(e) => {
                errs += 1;
                eprintln!("  {} {}: {}", paint("31", "fail"), u, e);
            }
        }
    }
    if errs == res.len() {
        Err("all repositories failed".into())
    } else {
        Ok(())
    }
}
