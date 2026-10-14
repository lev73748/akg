// Триггеры dpkg: interest/activate из info/*.triggers, запуск postinst triggered.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;

use crate::db::{field, Cfg, StatusDb};
use crate::install::run_script;
use crate::progress;
use crate::util::*;

#[derive(Default)]
struct Spec {
    interests: Vec<String>,
    activates: Vec<String>,
}

fn parse(text: &str) -> Spec {
    let mut s = Spec::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut it = line.split_whitespace();
        let Some(first) = it.next() else { continue };
        match (first, it.next()) {
            ("interest" | "interest-await" | "interest-noawait", Some(n)) => {
                s.interests.push(n.to_string())
            }
            ("activate" | "activate-await" | "activate-noawait", Some(n)) => {
                s.activates.push(n.to_string())
            }
            (n, None) if !n.starts_with("interest") && !n.starts_with("activate") => {
                s.interests.push(n.to_string())
            }
            _ => {}
        }
    }
    s
}

// Сработал ли файловый триггер: затронут путь внутри target (или файл ровно по target).
fn hit(touched: &[(String, bool)], target: &str) -> bool {
    let t = target.trim_end_matches('/');
    touched.iter().any(|(p, is_dir)| {
        if p.len() > t.len() {
            p.starts_with(t) && p.as_bytes()[t.len()] == b'/'
        } else {
            !*is_dir && p == t
        }
    })
}

pub fn run(cfg: &Cfg, db: &StatusDb, configured: &HashSet<String>, touched: &[(String, bool)]) {
    let Ok(rd) = fs::read_dir(cfg.info()) else { return };
    let mut specs: BTreeMap<String, Spec> = BTreeMap::new();
    for e in rd.flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        let Some(pkg) = n.strip_suffix(".triggers") else { continue };
        let Ok(text) = fs::read_to_string(e.path()) else { continue };
        specs.insert(pkg.to_string(), parse(&text));
    }
    if specs.is_empty() {
        return;
    }

    // явные триггеры, которые активировали только что настроенные пакеты
    let mut named: BTreeSet<&str> = BTreeSet::new();
    for p in configured {
        if let Some(s) = specs.get(p) {
            for a in &s.activates {
                named.insert(a.as_str());
            }
        }
    }

    let mut pending: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (pkg, s) in &specs {
        if configured.contains(pkg) || db.status_of(pkg).as_deref() != Some("installed") {
            continue;
        }
        for t in &s.interests {
            let fire = if t.starts_with('/') { hit(touched, t) } else { named.contains(t.as_str()) };
            if fire {
                pending.entry(pkg.clone()).or_default().insert(t.clone());
            }
        }
    }

    // только пакеты, у которых есть postinst
    let todo: Vec<(&String, &BTreeSet<String>)> = pending
        .iter()
        .filter(|(pkg, _)| cfg.info().join(format!("{pkg}.postinst")).exists())
        .collect();
    let total = todo.len();

    for (k, (pkg, names)) in todo.iter().enumerate() {
        progress::step("Trigger   ", k, total, pkg);
        let script = cfg.info().join(format!("{pkg}.postinst"));
        let arch = db.get(pkg).and_then(|s| field(s, "Architecture")).unwrap_or_default();
        let list = names.iter().cloned().collect::<Vec<_>>().join(" ");
        if let Err(e) = run_script(cfg, pkg, &arch, &script, "postinst", &["triggered", list.as_str()]) {
            progress::emit(&format!("{} {}", paint("33", "warning:"), e));
        }
    }
    progress::clear_line();
}
