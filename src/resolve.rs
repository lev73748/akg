// Свой резолвер зависимостей: установка, upgrade, удаление с зависимыми, autoremove.

use std::collections::{HashMap, HashSet};

use crate::repo::{Dep, Entry, Index};
use crate::util::R;
use crate::version::{relate, Version};

pub struct Request {
    pub name: String,
    pub version: Option<String>,
    pub reinstall: bool,
}

pub struct Plan {
    pub install: Vec<Entry>,
    pub remove: Vec<String>,
}

pub fn dep_ok(d: &Dep, v: &Version) -> bool {
    match &d.rel {
        None => true,
        Some((op, ver)) => relate(v, *op, ver),
    }
}

pub fn fmt_group(g: &[Dep]) -> String {
    g.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(" | ")
}

fn conflict_hit(c: &Dep, p: &Entry) -> bool {
    (p.name == c.name && dep_ok(c, &p.version))
        || (c.rel.is_none() && p.provides.iter().any(|(pn, _)| pn == &c.name))
}

struct Res<'a> {
    idx: &'a Index,
    inst: &'a HashMap<String, Entry>,
    chosen: HashMap<String, Entry>,
    order: Vec<String>,
    visiting: HashSet<String>,
    done: HashSet<String>,
    prov: HashMap<String, Vec<(String, Option<Version>)>>,
    remove: Vec<String>,
}

impl<'a> Res<'a> {
    fn new(idx: &'a Index, inst: &'a HashMap<String, Entry>) -> Self {
        let mut prov: HashMap<String, Vec<(String, Option<Version>)>> = HashMap::new();
        for p in inst.values() {
            for (pn, pv) in &p.provides {
                prov.entry(pn.clone()).or_default().push((p.name.clone(), pv.clone()));
            }
        }
        Res {
            idx,
            inst,
            chosen: HashMap::new(),
            order: Vec::new(),
            visiting: HashSet::new(),
            done: HashSet::new(),
            prov,
            remove: Vec::new(),
        }
    }

    fn eff(&self, name: &str) -> Option<&Entry> {
        self.chosen.get(name).or_else(|| self.inst.get(name))
    }

    fn provided(&self, d: &Dep) -> bool {
        if let Some(e) = self.eff(&d.name) {
            if dep_ok(d, &e.version) {
                return true;
            }
        }
        if let Some(ps) = self.prov.get(&d.name) {
            for (_, pv) in ps {
                match &d.rel {
                    None => return true,
                    Some((op, ver)) => {
                        if let Some(v) = pv {
                            if relate(v, *op, ver) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    fn pick(&self, d: &Dep) -> Option<Entry> {
        if let Some(list) = self.idx.by_name.get(&d.name) {
            for e in list {
                if dep_ok(d, &e.version) {
                    return Some(e.clone());
                }
            }
        }
        if d.rel.is_none() {
            if let Some(ps) = self.idx.providers.get(&d.name) {
                for (pn, _) in ps {
                    if let Some(e) = self.idx.best(pn) {
                        return Some(e.clone());
                    }
                }
            }
        }
        None
    }

    fn satisfy(&mut self, g: &[Dep], who: &str) -> R<()> {
        if g.iter().any(|d| self.provided(d)) {
            return Ok(());
        }
        for d in g {
            if let Some(c) = self.pick(d) {
                return self.add(&c);
            }
        }
        Err(format!("{who}: unmet dependency: {}", fmt_group(g)))
    }

    fn check_conflicts(&mut self, e: &Entry) -> R<()> {
        let mine: Vec<Dep> = e.conflicts.iter().chain(e.breaks.iter()).cloned().collect();
        let mut remove: Vec<String> = Vec::new();

        for c in &mine {
            let mut hits: Vec<(String, bool)> = Vec::new();
            for (n, p) in self.inst.iter() {
                if n == &e.name || self.chosen.contains_key(n) {
                    continue;
                }
                if conflict_hit(c, p) {
                    hits.push((n.clone(), true));
                }
            }
            for (n, p) in self.chosen.iter() {
                if n == &e.name {
                    continue;
                }
                if conflict_hit(c, p) {
                    hits.push((n.clone(), false));
                }
            }
            for (n, installed) in hits {
                if installed && e.replaces.iter().any(|r| r.name == n) {
                    if !remove.contains(&n) {
                        remove.push(n);
                    }
                } else {
                    return Err(format!("{} conflicts with {}", e.name, n));
                }
            }
        }

        for (n, p) in self.inst.iter() {
            if n == &e.name || self.chosen.contains_key(n) {
                continue;
            }
            for c in p.conflicts.iter().chain(p.breaks.iter()) {
                if c.name == e.name && dep_ok(c, &e.version) {
                    if e.replaces.iter().any(|r| &r.name == n) {
                        if !remove.contains(n) {
                            remove.push(n.clone());
                        }
                    } else {
                        return Err(format!("installed package {} conflicts with {}", n, e.name));
                    }
                    break;
                }
            }
        }

        for n in remove {
            if !self.remove.contains(&n) {
                self.remove.push(n);
            }
        }
        Ok(())
    }

    fn add(&mut self, e: &Entry) -> R<()> {
        if self.done.contains(&e.name) || self.visiting.contains(&e.name) {
            return Ok(());
        }
        self.visiting.insert(e.name.clone());
        for v in self.prov.values_mut() {
            v.retain(|(p, _)| p != &e.name);
        }
        for (pn, pv) in &e.provides {
            self.prov.entry(pn.clone()).or_default().push((e.name.clone(), pv.clone()));
        }
        self.chosen.insert(e.name.clone(), e.clone());
        self.check_conflicts(e)?;

        let groups: Vec<Vec<Dep>> = e.pre_depends.iter().chain(e.depends.iter()).cloned().collect();
        for g in &groups {
            self.satisfy(g, &e.name)?;
        }
        self.visiting.remove(&e.name);
        self.done.insert(e.name.clone());
        self.order.push(e.name.clone());
        Ok(())
    }
}

pub fn plan_install(idx: &Index, inst: &HashMap<String, Entry>, reqs: &[Request]) -> R<Plan> {
    let mut r = Res::new(idx, inst);
    for q in reqs {
        let found: Option<Entry> = match &q.version {
            Some(v) => idx.find(&q.name, v).cloned(),
            None => idx.best(&q.name).cloned().or_else(|| {
                if let Some(ps) = idx.providers.get(&q.name) {
                    for (pn, _) in ps {
                        if let Some(e) = idx.best(pn) {
                            return Some(e.clone());
                        }
                    }
                }
                None
            }),
        };
        let e = found.ok_or_else(|| format!("unable to locate package {}", q.name))?;
        if let Some(cur) = inst.get(&e.name) {
            if !q.reinstall && cur.version >= e.version {
                println!("{} is already the newest version ({})", e.name, cur.vstr);
                continue;
            }
        }
        r.add(&e)?;
    }
    let install: Vec<Entry> = r.order.iter().filter_map(|n| r.chosen.get(n).cloned()).collect();
    Ok(Plan { install, remove: r.remove })
}

pub fn upgrade_requests(idx: &Index, inst: &HashMap<String, Entry>) -> Vec<Request> {
    let mut v = Vec::new();
    for (n, p) in inst {
        if let Some(b) = idx.best(n) {
            if b.version > p.version {
                v.push(Request { name: n.clone(), version: None, reinstall: false });
            }
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

// ------------------------------------------------------------ removal

fn satisfiers(inst: &HashMap<String, Entry>, d: &Dep) -> Vec<String> {
    let mut v = Vec::new();
    for (n, p) in inst {
        let direct = &p.name == &d.name && dep_ok(d, &p.version);
        let virt = d.rel.is_none() && p.provides.iter().any(|(pn, _)| pn == &d.name);
        if direct || virt {
            v.push(n.clone());
        }
    }
    v
}

/// Целевые пакеты + все установленные, которые без них перестанут работать.
pub fn removal_closure(inst: &HashMap<String, Entry>, targets: &[String]) -> Vec<String> {
    let mut set: HashSet<String> = targets.iter().cloned().collect();
    loop {
        let mut added: Vec<String> = Vec::new();
        for (n, p) in inst {
            if set.contains(n) {
                continue;
            }
            for g in p.pre_depends.iter().chain(p.depends.iter()) {
                let had = g.iter().any(|d| !satisfiers(inst, d).is_empty());
                let alive = g
                    .iter()
                    .any(|d| satisfiers(inst, d).iter().any(|s| !set.contains(s)));
                if had && !alive {
                    added.push(n.clone());
                    break;
                }
            }
        }
        if added.is_empty() {
            break;
        }
        set.extend(added);
    }
    let mut v: Vec<String> = set.into_iter().collect();
    v.sort();
    v
}

/// Установленные пакеты с меткой auto, до которых не достать от ручных.
pub fn autoremove_set(inst: &HashMap<String, Entry>, auto: &HashSet<String>) -> Vec<String> {
    let mut keep: HashSet<String> = HashSet::new();
    let mut stack: Vec<String> = Vec::new();
    for (n, p) in inst {
        if !auto.contains(n) || p.essential || p.priority == "required" {
            if keep.insert(n.clone()) {
                stack.push(n.clone());
            }
        }
    }
    while let Some(n) = stack.pop() {
        let p = match inst.get(&n) {
            Some(p) => p,
            None => continue,
        };
        for g in p.pre_depends.iter().chain(p.depends.iter()) {
            for d in g {
                for s in satisfiers(inst, d) {
                    if keep.insert(s.clone()) {
                        stack.push(s);
                    }
                }
            }
        }
    }
    let mut v: Vec<String> = inst.keys().filter(|n| !keep.contains(*n)).cloned().collect();
    v.sort();
    v
}

fn visit(
    n: &String,
    inst: &HashMap<String, Entry>,
    in_set: &HashSet<String>,
    seen: &mut HashSet<String>,
    out: &mut Vec<String>,
) {
    if !seen.insert(n.clone()) {
        return;
    }
    if let Some(p) = inst.get(n) {
        for g in p.pre_depends.iter().chain(p.depends.iter()) {
            for d in g {
                for s in satisfiers(inst, d) {
                    if in_set.contains(&s) {
                        visit(&s, inst, in_set, seen, out);
                    }
                }
            }
        }
    }
    out.push(n.clone());
}

/// Порядок удаления: сначала зависимые, потом то, от чего они зависели.
pub fn removal_order(inst: &HashMap<String, Entry>, set: &[String]) -> Vec<String> {
    let in_set: HashSet<String> = set.iter().cloned().collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for n in set {
        visit(n, inst, &in_set, &mut seen, &mut out);
    }
    out.reverse();
    out
}
