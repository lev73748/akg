// База: Cfg, блокировка, stanza/status, журнал updates/, метки auto-installed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::repo::{parse_entry, Entry};
use crate::util::*;

pub struct Cfg {
    pub root: String,
    pub prefix: String,
    pub jobs: usize,
    pub yes: bool,
    pub dry_run: bool,
    pub force_overwrite: bool,
    pub no_scripts: bool,
}

impl Cfg {
    pub fn fs(&self, name: &str) -> PathBuf {
        PathBuf::from(format!("{}{}", self.root, name))
    }
    pub fn admin(&self) -> PathBuf {
        self.fs(&format!("{}/var/lib/dpkg", self.prefix))
    }
    pub fn info(&self) -> PathBuf {
        self.admin().join("info")
    }
    pub fn archives(&self) -> PathBuf {
        self.fs(&format!("{}/var/cache/akg/archives", self.prefix))
    }
    pub fn marks_path(&self) -> PathBuf {
        self.fs(&format!("{}/var/lib/apt/extended_states", self.prefix))
    }
    pub fn scripts_enabled(&self) -> bool {
        !self.no_scripts && self.root.is_empty()
    }
}

// ------------------------------------------------------------ lock

pub struct Lock {
    _fds: Vec<File>,
}

pub fn lock_db(cfg: &Cfg) -> R<Lock> {
    let dir = cfg.admin();
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut fds = Vec::new();
    for name in ["lock-frontend", "lock"] {
        let p = dir.join(name);
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let mut fl: libc::flock = unsafe { std::mem::zeroed() };
        fl.l_type = libc::F_WRLCK as _;
        fl.l_whence = libc::SEEK_SET as _;
        fl.l_start = 0;
        fl.l_len = 0;
        let rc = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_SETLK, &fl as *const libc::flock) };
        if rc != 0 {
            return Err(format!("cannot lock {} (is apt/dpkg/akg already running?)", p.display()));
        }
        fds.push(f);
    }
    Ok(Lock { _fds: fds })
}

// ------------------------------------------------------------ control / stanza

pub struct Control {
    pub fields: Vec<(String, String)>,
}

impl Control {
    pub fn parse(text: &str) -> Control {
        let mut f: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                if let Some(last) = f.last_mut() {
                    last.1.push('\n');
                    last.1.push_str(line);
                }
                continue;
            }
            if line.trim().is_empty() {
                if f.is_empty() {
                    continue;
                }
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                f.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        Control { fields: f }
    }

    pub fn get(&self, k: &str) -> Option<&str> {
        self.fields.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

const FIELD_ORDER: &[&str] = &[
    "Package", "Essential", "Status", "Priority", "Section", "Installed-Size", "Origin",
    "Maintainer", "Bugs", "Architecture", "Multi-Arch", "Source", "Version", "Replaces",
    "Provides", "Depends", "Pre-Depends", "Recommends", "Suggests", "Breaks", "Conflicts",
    "Enhances",
];

pub fn build_stanza(ctrl: &Control, status: &str, conf: &[(String, String)]) -> String {
    let mut s = String::new();
    for key in FIELD_ORDER {
        if *key == "Status" {
            s.push_str(&format!("Status: {status}\n"));
            continue;
        }
        if let Some(v) = ctrl.get(key) {
            s.push_str(key);
            s.push_str(": ");
            s.push_str(v);
            s.push('\n');
        }
    }
    if !conf.is_empty() {
        s.push_str("Conffiles:\n");
        for (n, h) in conf {
            s.push_str(&format!(" {n} {h}\n"));
        }
    }
    if let Some(v) = ctrl.get("Description") {
        s.push_str("Description: ");
        s.push_str(v);
        s.push('\n');
    }
    for (k, v) in &ctrl.fields {
        if FIELD_ORDER.iter().any(|x| x.eq_ignore_ascii_case(k))
            || k.eq_ignore_ascii_case("Description")
            || k.eq_ignore_ascii_case("Conffiles")
        {
            continue;
        }
        s.push_str(&format!("{k}: {v}\n"));
    }
    s
}

pub fn field(st: &str, key: &str) -> Option<String> {
    for line in st.lines() {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case(key) && !k.starts_with(' ') {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

pub fn conffiles_of(st: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let mut inside = false;
    for line in st.lines() {
        if inside {
            if line.starts_with(' ') || line.starts_with('\t') {
                let mut p = line.split_whitespace();
                if let (Some(n), Some(h)) = (p.next(), p.next()) {
                    v.push((n.to_string(), h.to_string()));
                }
                continue;
            }
            inside = false;
        }
        if line.starts_with("Conffiles:") {
            inside = true;
        }
    }
    v
}

pub fn set_status(st: &str, status: &str) -> String {
    let mut out = String::new();
    for line in st.lines() {
        if line.starts_with("Status:") {
            out.push_str(&format!("Status: {status}\n"));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub struct OldInfo {
    pub version: String,
    pub conf: HashMap<String, String>,
}

pub fn old_info(st: &str) -> Option<OldInfo> {
    let status = field(st, "Status")?;
    let s = status.split_whitespace().nth(2)?;
    if s == "not-installed" || s == "config-files" {
        return None;
    }
    Some(OldInfo {
        version: field(st, "Version").unwrap_or_default(),
        conf: conffiles_of(st).into_iter().collect(),
    })
}

// ------------------------------------------------------------ status database

pub struct StatusDb {
    pub order: Vec<String>,
    pub map: HashMap<String, String>,
}

impl StatusDb {
    pub fn load(path: &Path) -> R<StatusDb> {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let mut db = StatusDb { order: Vec::new(), map: HashMap::new() };
        db.add_text(&String::from_utf8_lossy(&bytes));
        Ok(db)
    }

    pub fn add_text(&mut self, text: &str) {
        let mut cur = String::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                self.flush(&mut cur);
            } else {
                cur.push_str(line);
                cur.push('\n');
            }
        }
        self.flush(&mut cur);
    }

    fn flush(&mut self, cur: &mut String) {
        if cur.is_empty() {
            return;
        }
        match field(cur.as_str(), "Package") {
            Some(name) => {
                let st = std::mem::take(cur);
                self.set(&name, st);
            }
            None => cur.clear(),
        }
    }

    pub fn set(&mut self, name: &str, mut stanza: String) {
        if !stanza.ends_with('\n') {
            stanza.push('\n');
        }
        if !self.map.contains_key(name) {
            self.order.push(name.to_string());
        }
        self.map.insert(name.to_string(), stanza);
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.map.get(name)
    }

    pub fn remove(&mut self, name: &str) {
        self.map.remove(name);
        self.order.retain(|n| n != name);
    }

    pub fn status_of(&self, name: &str) -> Option<String> {
        let st = self.map.get(name)?;
        let s = field(st, "Status")?;
        s.split_whitespace().nth(2).map(|x| x.to_string())
    }

    pub fn installed(&self) -> HashMap<String, Entry> {
        let mut m = HashMap::new();
        for (n, st) in &self.map {
            if let Some(e) = parse_entry(st) {
                if matches!(
                    e.status.as_str(),
                    "installed" | "unpacked" | "half-configured" | "half-installed"
                        | "triggers-pending" | "triggers-awaited"
                ) {
                    m.insert(n.clone(), e);
                }
            }
        }
        m
    }

    pub fn save(&self, path: &Path) -> R<()> {
        let tmp = with_suffix(path, ".akg-new");
        let f = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let mut w = BufWriter::new(f);
        for n in &self.order {
            if let Some(st) = self.map.get(n) {
                w.write_all(st.as_bytes()).map_err(|e| e.to_string())?;
                w.write_all(b"\n").map_err(|e| e.to_string())?;
            }
        }
        w.flush().map_err(|e| e.to_string())?;
        w.get_ref().sync_all().map_err(|e| e.to_string())?;
        drop(w);
        fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }
}

pub fn update_files(cfg: &Cfg) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(cfg.admin().join("updates")) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) {
                names.push(n);
            }
        }
    }
    names.sort();
    names
}

pub fn incorporate_updates(cfg: &Cfg, db: &mut StatusDb) -> R<()> {
    let names = update_files(cfg);
    if names.is_empty() {
        return Ok(());
    }
    eprintln!("{}", paint("33", "akg: incorporating leftover updates journal..."));
    let dir = cfg.admin().join("updates");
    for n in &names {
        let b = fs::read(dir.join(n)).map_err(|e| format!("{n}: {e}"))?;
        db.add_text(&String::from_utf8_lossy(&b));
    }
    db.save(&cfg.admin().join("status"))?;
    for n in &names {
        let _ = fs::remove_file(dir.join(n));
    }
    Ok(())
}

// ------------------------------------------------------------ auto-installed marks (apt-совместимо)

pub struct Marks {
    map: BTreeMap<String, (String, bool)>,
}

impl Marks {
    pub fn load(cfg: &Cfg) -> Marks {
        let text = fs::read_to_string(cfg.marks_path()).unwrap_or_default();
        let mut map = BTreeMap::new();
        for block in text.split("\n\n") {
            let (mut name, mut arch, mut auto) = (String::new(), String::new(), false);
            for line in block.lines() {
                if let Some((k, v)) = line.split_once(':') {
                    match k.trim() {
                        "Package" => name = v.trim().to_string(),
                        "Architecture" => arch = v.trim().to_string(),
                        "Auto-Installed" => auto = v.trim() == "1",
                        _ => {}
                    }
                }
            }
            if !name.is_empty() {
                map.insert(name, (arch, auto));
            }
        }
        Marks { map }
    }

    pub fn set(&mut self, name: &str, arch: &str, auto: bool) {
        self.map.insert(name.to_string(), (arch.to_string(), auto));
    }

    pub fn remove(&mut self, name: &str) {
        self.map.remove(name);
    }

    pub fn autos(&self) -> HashSet<String> {
        self.map.iter().filter(|(_, v)| v.1).map(|(n, _)| n.clone()).collect()
    }

    pub fn save(&self, cfg: &Cfg) -> R<()> {
        let mut out = String::new();
        for (n, (a, au)) in &self.map {
            out.push_str(&format!("Package: {n}\n"));
            if !a.is_empty() {
                out.push_str(&format!("Architecture: {a}\n"));
            }
            out.push_str(&format!("Auto-Installed: {}\n\n", if *au { 1 } else { 0 }));
        }
        let p = cfg.marks_path();
        if let Some(d) = p.parent() {
            let _ = fs::create_dir_all(d);
        }
        write_atomic(&p, out.as_bytes())
    }
}
