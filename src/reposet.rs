// Наборы репозиториев (main / root / x11 / tur / свои): разбор, проверка подписи,
// запись в etc/apt, выбор по умолчанию (akg bench, add-repo, repo-choice).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use crate::repo::{self, Source};
use crate::util::*;

// Адрес из tur-repo (sources.list.d/tur.list). Подпись проверяется перед записью.
pub const TUR_URI: &str = "https://tur.kcubeterm.com";

#[derive(Clone)]
pub struct RepoSet {
    pub name: String,
    pub members: Vec<Source>,
    // true: набор добавлен самим пользователем, root/x11 из него создаются
    // false: зеркало из termux-tools, root/x11 только обновляются, если уже есть
    pub user: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Main,
    Root,
    X11,
    Tur,
    Other,
}

pub fn kind(s: &Source) -> Kind {
    let c0 = s.comps.first().map(|c| c.as_str()).unwrap_or("");
    if s.suite == "stable" && c0 == "main" {
        Kind::Main
    } else if s.suite == "root" && c0 == "stable" {
        Kind::Root
    } else if s.suite == "x11" && c0 == "main" {
        Kind::X11
    } else if s.suite.starts_with("tur-") {
        Kind::Tur
    } else {
        Kind::Other
    }
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Main => "main",
        Kind::Root => "root",
        Kind::X11 => "x11",
        Kind::Tur => "tur",
        Kind::Other => "other",
    }
}

fn parts(set: &RepoSet) -> String {
    set.members
        .iter()
        .map(|m| kind_name(kind(m)))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn host_of(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}

pub fn display_name(uri: &str) -> String {
    let s = uri
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    for suf in ["/termux-main", "/main"] {
        if let Some(h) = s.strip_suffix(suf) {
            return h.to_string();
        }
    }
    s.to_string()
}

// ------------------------------------------------------------ проверка ввода

fn valid_token(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-' || b == b'+')
}

fn valid_url(u: &str) -> bool {
    if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 300 {
        return false;
    }
    let ok = u
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&()*+,;=%".contains(&b));
    ok && !host_of(u).is_empty()
}

fn guess(uri: &str) -> (String, Vec<String>) {
    let low = uri.to_ascii_lowercase();
    let last = low.rsplit('/').next().unwrap_or("");
    if low.contains("tur.kcubeterm") || low.contains("termux-user-repository") || last == "tur" {
        (
            "tur-packages".to_string(),
            vec!["tur".into(), "tur-on-device".into(), "tur-continuous".into()],
        )
    } else if last.contains("x11") {
        ("x11".to_string(), vec!["main".into()])
    } else if last.contains("root") {
        ("root".to_string(), vec!["stable".into()])
    } else {
        ("stable".to_string(), vec!["main".into()])
    }
}

/// "ссылка" или "ссылка suite comp [comp...]"
pub fn parse_member(spec: &str) -> R<Source> {
    let mut t = spec.split_whitespace();
    let raw = t.next().ok_or("empty repository entry")?;
    let uri = repo::normalize_repo(raw);
    if !valid_url(&uri) {
        return Err(format!("{raw}: not a valid repository link"));
    }
    let rest: Vec<&str> = t.collect();
    let (suite, comps) = if rest.is_empty() {
        guess(&uri)
    } else if rest.len() >= 2 {
        (
            rest[0].to_string(),
            rest[1..].iter().map(|s| s.to_string()).collect(),
        )
    } else {
        return Err(format!("{raw}: give both suite and component, or neither"));
    };
    if !valid_token(&suite) || comps.is_empty() || !comps.iter().all(|c| valid_token(c)) {
        return Err(format!("{raw}: bad suite/component name"));
    }
    Ok(Source {
        uri: uri.trim_end_matches('/').to_string(),
        suite,
        comps,
        signed_by: None,
    })
}

pub fn tur_source() -> Source {
    Source {
        uri: TUR_URI.to_string(),
        suite: "tur-packages".to_string(),
        comps: vec!["tur".into(), "tur-on-device".into(), "tur-continuous".into()],
        signed_by: None,
    }
}

// root и x11 рядом с main: termux-main -> termux-root / termux-x11, main -> root / x11
fn siblings(main: &Source) -> Vec<Source> {
    let Some((head, last)) = main.uri.rsplit_once('/') else {
        return Vec::new();
    };
    let (r, x) = match last {
        "termux-main" => ("termux-root", "termux-x11"),
        "main" => ("root", "x11"),
        _ => return Vec::new(),
    };
    vec![
        Source {
            uri: format!("{head}/{r}"),
            suite: "root".into(),
            comps: vec!["stable".into()],
            signed_by: None,
        },
        Source {
            uri: format!("{head}/{x}"),
            suite: "x11".into(),
            comps: vec!["main".into()],
            signed_by: None,
        },
    ]
}

pub fn single_main(uri: &str) -> RepoSet {
    RepoSet {
        name: display_name(uri),
        members: vec![Source {
            uri: uri.trim_end_matches('/').to_string(),
            suite: "stable".into(),
            comps: vec!["main".into()],
            signed_by: None,
        }],
        user: false,
    }
}

fn main_uri(set: &RepoSet) -> Option<String> {
    set.members
        .iter()
        .find(|m| kind(m) == Kind::Main)
        .map(|m| m.uri.clone())
}

fn auto_name(set: &RepoSet) -> String {
    match main_uri(set) {
        Some(u) => display_name(&u),
        None => set
            .members
            .first()
            .map(|m| display_name(&m.uri))
            .unwrap_or_else(|| "repositories".to_string()),
    }
}

fn clean_name(n: &str) -> String {
    n.chars()
        .filter(|c| !c.is_control() && *c != '[' && *c != ']')
        .collect()
}

// ------------------------------------------------------------ подпись

/// Скачивает InRelease/Release и проверяет подпись через gpgv тем же кодом, что и update.
pub fn verify(prefix: &Path, s: &Source) -> R<()> {
    repo::fetch_release(prefix, s, &s.uri).map(|_| ())
}

// ------------------------------------------------------------ наборы: файлы

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 3 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let Ok(md) = fs::metadata(&p) else {
            continue;
        };
        if md.is_dir() {
            walk(&p, depth + 1, out);
        } else {
            out.push(p);
        }
    }
}

// Файл зеркала Termux: MAIN="..." ROOT="..." X11="..."
fn parse_mirror_file(text: &str) -> Option<RepoSet> {
    let (mut main, mut root, mut x11): (Option<String>, Option<String>, Option<String>) =
        (None, None, None);
    for line in text.lines() {
        let line = line.trim();
        let val = |k: &str| -> Option<String> {
            line.strip_prefix(k).map(|v| {
                v.trim()
                    .trim_matches(|c: char| c == '"' || c == '\'')
                    .trim_end_matches('/')
                    .to_string()
            })
        };
        if let Some(v) = val("MAIN=") {
            main = Some(v);
        } else if let Some(v) = val("ROOT=") {
            root = Some(v);
        } else if let Some(v) = val("X11=") {
            x11 = Some(v);
        }
    }
    let main = main?;
    if !valid_url(&main) {
        return None;
    }
    let mut members = vec![Source {
        uri: main.clone(),
        suite: "stable".into(),
        comps: vec!["main".into()],
        signed_by: None,
    }];
    if let Some(r) = root.filter(|u| valid_url(u)) {
        members.push(Source {
            uri: r,
            suite: "root".into(),
            comps: vec!["stable".into()],
            signed_by: None,
        });
    }
    if let Some(x) = x11.filter(|u| valid_url(u)) {
        members.push(Source {
            uri: x,
            suite: "x11".into(),
            comps: vec!["main".into()],
            signed_by: None,
        });
    }
    Some(RepoSet {
        name: display_name(&main),
        members,
        user: false,
    })
}

pub fn mirror_sets(prefix: &Path) -> Vec<RepoSet> {
    let mut files = Vec::new();
    walk(&prefix.join("etc/termux/mirrors"), 0, &mut files);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for p in files {
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        let Some(set) = parse_mirror_file(&text) else {
            continue;
        };
        if seen.insert(set.members[0].uri.clone()) {
            out.push(set);
        }
    }
    out
}

/// Формат: строки-ссылки; "[имя]" начинает новый набор; "#" — комментарий.
/// Ссылки без заголовка образуют один набор.
pub fn parse_sets(text: &str) -> R<Vec<RepoSet>> {
    let mut out: Vec<RepoSet> = Vec::new();
    let mut cur: Option<RepoSet> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if let Some(s) = cur.take() {
                out.push(s);
            }
            cur = Some(RepoSet {
                name: clean_name(name.trim()),
                members: Vec::new(),
                user: true,
            });
            continue;
        }
        let m = parse_member(line).map_err(|e| format!("line {}: {e}", n + 1))?;
        match cur.as_mut() {
            Some(s) => s.members.push(m),
            None => {
                cur = Some(RepoSet {
                    name: String::new(),
                    members: vec![m],
                    user: true,
                })
            }
        }
    }
    if let Some(s) = cur.take() {
        out.push(s);
    }
    out.retain(|s| !s.members.is_empty());
    for s in &mut out {
        if s.name.is_empty() {
            let n = auto_name(s);
            s.name = n;
        }
    }
    Ok(out)
}

fn user_path(prefix: &Path) -> PathBuf {
    prefix.join("etc/akg/repos.list")
}

fn user_sets(prefix: &Path) -> Vec<RepoSet> {
    let Ok(text) = fs::read_to_string(user_path(prefix)) else {
        return Vec::new();
    };
    match parse_sets(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{} etc/akg/repos.list: {e}", paint("33", "warning:"));
            Vec::new()
        }
    }
}

fn save_user_sets(prefix: &Path, sets: &[RepoSet]) -> R<()> {
    let p = user_path(prefix);
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut s = String::from("# akg repository sets (akg add-repo / akg repo-choice)\n");
    for set in sets {
        s.push_str(&format!("\n[{}]\n", clean_name(&set.name)));
        for m in &set.members {
            s.push_str(&format!("{} {} {}\n", m.uri, m.suite, m.comps.join(" ")));
        }
    }
    write_atomic(&p, s.as_bytes())
}

pub fn all_sets(prefix: &Path) -> Vec<RepoSet> {
    let mut out: Vec<RepoSet> = user_sets(prefix);
    let mut seen: HashSet<String> = out.iter().filter_map(main_uri).collect();
    for s in mirror_sets(prefix) {
        let key = main_uri(&s);
        if let Some(u) = &key {
            if !seen.insert(u.clone()) {
                continue;
            }
        }
        out.push(s);
    }
    out
}

/// main-репозиторий, который сейчас стоит в sources.list
pub fn current_main(prefix: &Path) -> Option<String> {
    repo::read_sources(prefix)
        .iter()
        .find(|s| kind(s) == Kind::Main)
        .map(|s| s.uri.trim_end_matches('/').to_string())
}

// ------------------------------------------------------------ запись в etc/apt

fn kind_file(prefix: &Path, k: Kind) -> Option<PathBuf> {
    let apt = prefix.join("etc/apt");
    match k {
        Kind::Main => Some(apt.join("sources.list")),
        Kind::Root => Some(apt.join("sources.list.d/root.list")),
        Kind::X11 => Some(apt.join("sources.list.d/x11.list")),
        Kind::Tur => Some(apt.join("sources.list.d/tur.list")),
        Kind::Other => None,
    }
}

fn target_file(prefix: &Path, s: &Source) -> PathBuf {
    match kind_file(prefix, kind(s)) {
        Some(p) => p,
        None => prefix.join(format!(
            "etc/apt/sources.list.d/akg-{}.list",
            repo::san(&host_of(&s.uri))
        )),
    }
}

fn write_list(path: &Path, members: &[&Source]) -> R<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let bak = with_suffix(path, ".akg-bak");
    if path.exists() && !bak.exists() {
        let _ = fs::copy(path, &bak);
    }
    let mut s = String::from("# managed by akg (akg bench / repo-choice / add-repo)\n");
    for m in members {
        s.push_str(&format!("deb {} {} {}\n", m.uri, m.suite, m.comps.join(" ")));
    }
    write_atomic(path, s.as_bytes())
}

// индексы старых зеркал удаляем: Index::load читает все файлы каталога
fn prune_lists(prefix: &Path) {
    let keep: Vec<String> = repo::read_sources(prefix)
        .iter()
        .map(|s| format!("{}_", repo::san(s.uri.trim_end_matches('/'))))
        .collect();
    if let Ok(rd) = fs::read_dir(repo::lists_dir(prefix)) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !keep.iter().any(|k| name.starts_with(k.as_str())) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// Делает набор репозиториев основным: проверяет подписи, добавляет TUR, пишет файлы.
pub fn apply(prefix: &Path, set: &RepoSet) -> R<()> {
    if repo::is_custom() {
        return Err("cannot change the default repository together with --repo".into());
    }
    let skip_new = |m: &Source| {
        !set.user
            && matches!(kind(m), Kind::Root | Kind::X11)
            && !target_file(prefix, m).exists()
    };
    let wanted: Vec<&Source> = set.members.iter().filter(|m| !skip_new(m)).collect();

    eprintln!("{} {}", paint("36", "akg: verifying"), set.name);
    let results: Vec<R<()>> = wanted.par_iter().map(|m| verify(prefix, m)).collect();

    let mut chosen: Vec<Source> = Vec::new();
    for (m, r) in wanted.iter().zip(results.iter()) {
        match r {
            Ok(()) => chosen.push((*m).clone()),
            Err(e) => {
                if kind(m) == Kind::Main {
                    return Err(format!("{}: {e}", m.uri));
                }
                eprintln!("{} {} skipped: {e}", paint("33", "warning:"), m.uri);
            }
        }
    }

    // TUR включается всегда, но только если подпись сошлась
    if !chosen.iter().any(|m| kind(m) == Kind::Tur) {
        let tur = tur_source();
        match verify(prefix, &tur) {
            Ok(()) => chosen.push(tur),
            Err(e) => eprintln!(
                "{} tur not added: {e}\n  (the key comes with the tur-repo package: akg install tur-repo)",
                paint("33", "warning:")
            ),
        }
    }
    if chosen.is_empty() {
        return Err("nothing to write".into());
    }

    let mut by: BTreeMap<PathBuf, Vec<&Source>> = BTreeMap::new();
    for m in &chosen {
        by.entry(target_file(prefix, m)).or_default().push(m);
    }
    for (path, ms) in &by {
        write_list(path, ms)?;
        println!("  wrote {}", path.display());
    }
    for k in [Kind::Root, Kind::X11] {
        if !chosen.iter().any(|m| kind(m) == k) {
            if let Some(p) = kind_file(prefix, k) {
                if p.exists() {
                    eprintln!(
                        "{} {} not changed (no {} repository in this set)",
                        paint("33", "warning:"),
                        p.display(),
                        kind_name(k)
                    );
                }
            }
        }
    }

    println!(
        "{} {} ({})",
        paint("1;32", "default repository:"),
        set.name,
        chosen
            .iter()
            .map(|m| kind_name(kind(m)))
            .collect::<Vec<_>>()
            .join(" ")
    );

    prune_lists(prefix);
    if let Err(e) = repo::update(prefix) {
        eprintln!("{} update: {e}", paint("33", "warning:"));
    }
    Ok(())
}

// ------------------------------------------------------------ ввод

pub fn interactive() -> bool {
    io::stdin().is_terminal() && io::stderr().is_terminal()
}

pub enum Choice {
    Index(usize), // с единицы
    Text(String),
    Cancel,
}

pub fn pick(n: usize, default: usize, allow_text: bool) -> Choice {
    let stdin = io::stdin();
    for _ in 0..3 {
        let extra = if allow_text { ", or paste a link" } else { "" };
        eprint!("Select [1-{n}, Enter = {default}, q = cancel{extra}]: ");
        let _ = io::stderr().flush();
        let mut l = String::new();
        match stdin.lock().read_line(&mut l) {
            Ok(0) | Err(_) => return Choice::Cancel,
            Ok(_) => {}
        }
        let l = l.trim();
        if l.is_empty() {
            return Choice::Index(default);
        }
        if l.eq_ignore_ascii_case("q") {
            return Choice::Cancel;
        }
        if let Ok(k) = l.parse::<usize>() {
            if k >= 1 && k <= n {
                return Choice::Index(k);
            }
            eprintln!("number out of range");
            continue;
        }
        if allow_text {
            return Choice::Text(l.to_string());
        }
        eprintln!("enter a number");
    }
    Choice::Cancel
}

fn ask_yes(prompt: &str, yes: bool) -> bool {
    if yes {
        return true;
    }
    if !interactive() {
        return false;
    }
    eprint!("{prompt} [Y/n] ");
    let _ = io::stderr().flush();
    let mut l = String::new();
    if io::stdin().read_line(&mut l).is_err() {
        return false;
    }
    let l = l.trim().to_lowercase();
    !(l == "n" || l == "no")
}

// ------------------------------------------------------------ команды

// Проверяет все ссылки набора (ошибка = набор не принимается). Если дана одна ссылка
// на main, root и x11 рядом проверяются и добавляются, когда они есть.
fn verify_set(prefix: &Path, set: &mut RepoSet) -> R<()> {
    eprintln!("{} {}", paint("36", "akg: verifying"), set.name);
    let results: Vec<R<()>> = set.members.par_iter().map(|m| verify(prefix, m)).collect();
    let mut errs: Vec<String> = Vec::new();
    for (m, r) in set.members.iter().zip(results.iter()) {
        match r {
            Ok(()) => eprintln!("  {} {}", paint("32", "ok"), m.uri),
            Err(e) => errs.push(format!("{}: {e}", m.uri)),
        }
    }
    if !errs.is_empty() {
        return Err(format!(
            "{}\nonly signed repositories are accepted: put the key into etc/apt/trusted.gpg.d/ (--no-verify turns the check off and is NOT safe)",
            errs.join("\n")
        ));
    }
    if set.members.len() == 1 && kind(&set.members[0]) == Kind::Main {
        let found: Vec<(Source, R<()>)> = siblings(&set.members[0])
            .into_par_iter()
            .map(|s| {
                let r = verify(prefix, &s);
                (s, r)
            })
            .collect();
        for (s, r) in found {
            match r {
                Ok(()) => {
                    eprintln!("  {} {}", paint("32", "ok"), s.uri);
                    set.members.push(s);
                }
                Err(e) => eprintln!(
                    "  skipped {}: {}",
                    s.uri,
                    e.lines().next().unwrap_or("not available")
                ),
            }
        }
    }
    Ok(())
}

pub fn add_repo(prefix: &Path, args: &[String], yes: bool, make_default: bool) -> R<()> {
    if args.is_empty() {
        return Err("add-repo: give a repository link, several links, or a file with links".into());
    }
    let mut sets: Vec<RepoSet> = Vec::new();
    let mut links: Vec<&String> = Vec::new();
    for a in args {
        if Path::new(a).is_file() {
            let text = fs::read_to_string(a).map_err(|e| format!("{a}: {e}"))?;
            let mut v = parse_sets(&text).map_err(|e| format!("{a}: {e}"))?;
            if v.is_empty() {
                return Err(format!("{a}: no repository links found"));
            }
            sets.append(&mut v);
        } else {
            links.push(a);
        }
    }
    if !links.is_empty() {
        let mut members = Vec::new();
        for l in links {
            members.push(parse_member(l)?);
        }
        sets.push(RepoSet {
            name: String::new(),
            members,
            user: true,
        });
    }

    for set in &mut sets {
        if set.name.is_empty() {
            let n = auto_name(set);
            set.name = n;
        }
        verify_set(prefix, set)?;
    }

    let mut store = user_sets(prefix);
    for s in &sets {
        let key = main_uri(s);
        store.retain(|o| !(o.name == s.name || (key.is_some() && main_uri(o) == key)));
        store.push(s.clone());
    }
    save_user_sets(prefix, &store)?;
    for s in &sets {
        println!("added: {} ({})", paint("1;32", &s.name), parts(s));
    }

    if sets.len() == 1 {
        if make_default || ask_yes("Use it as the default repository?", yes) {
            apply(prefix, &sets[0])?;
        }
    } else {
        println!("run 'akg repo-choice' to pick the default");
    }
    Ok(())
}

pub fn repo_choice(prefix: &Path, yes: bool) -> R<()> {
    let sets = all_sets(prefix);
    if sets.is_empty() {
        return Err(
            "no repository sets (etc/termux/mirrors is empty and nothing was added with add-repo)"
                .into(),
        );
    }
    let cur = current_main(prefix);
    let w = sets
        .iter()
        .map(|s| s.name.chars().count())
        .max()
        .unwrap_or(10)
        .max(10);
    println!(
        "{}",
        paint("1;33", &format!("{:>3}  {:<w$}  {}", "#", "repository", "parts", w = w))
    );
    let mut default = 1;
    for (i, s) in sets.iter().enumerate() {
        let is_cur = cur.is_some() && main_uri(s) == cur;
        if is_cur {
            default = i + 1;
        }
        println!(
            "{:>3}  {:<w$}  {}{}",
            i + 1,
            s.name,
            parts(s),
            if is_cur { "  (current)" } else { "" },
            w = w
        );
    }
    println!();
    if yes || !interactive() {
        return Err("repo-choice needs a terminal".into());
    }
    match pick(sets.len(), default, true) {
        Choice::Cancel => {
            println!("cancelled");
            Ok(())
        }
        Choice::Index(k) => apply(prefix, &sets[k - 1]),
        Choice::Text(t) => add_repo(prefix, &[t], false, true),
    }
}
