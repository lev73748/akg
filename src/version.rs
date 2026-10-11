// Сравнение версий — точный порт verrevcmp() из dpkg lib/dpkg/version.c

use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug)]
pub struct Version {
    pub epoch: u32,
    pub upstream: String,
    pub revision: String,
}

impl Version {
    pub fn parse(s: &str) -> Version {
        let s = s.trim();
        let (epoch, rest) = match s.split_once(':') {
            Some((e, r)) if !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit()) => {
                (e.parse().unwrap_or(0), r)
            }
            _ => (0, s),
        };
        let (up, rev) = match rest.rsplit_once('-') {
            Some((u, r)) => (u, r),
            None => (rest, ""),
        };
        Version {
            epoch,
            upstream: up.to_string(),
            revision: rev.to_string(),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.epoch != 0 {
            write!(f, "{}:", self.epoch)?;
        }
        write!(f, "{}", self.upstream)?;
        if !self.revision.is_empty() {
            write!(f, "-{}", self.revision)?;
        }
        Ok(())
    }
}

fn order(c: u8) -> i32 {
    if c.is_ascii_digit() {
        0
    } else if c.is_ascii_alphabetic() {
        c as i32
    } else if c == b'~' {
        -1
    } else if c != 0 {
        c as i32 + 256
    } else {
        0
    }
}

fn at(s: &[u8], k: usize) -> u8 {
    if k < s.len() {
        s[k]
    } else {
        0
    }
}

fn verrevcmp(a: &str, b: &str) -> i32 {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() || j < b.len() {
        let mut first_diff = 0i32;

        while (at(a, i) != 0 && !at(a, i).is_ascii_digit())
            || (at(b, j) != 0 && !at(b, j).is_ascii_digit())
        {
            let ac = order(at(a, i));
            let bc = order(at(b, j));
            if ac != bc {
                return ac - bc;
            }
            i += 1;
            j += 1;
        }
        while at(a, i) == b'0' {
            i += 1;
        }
        while at(b, j) == b'0' {
            j += 1;
        }
        while at(a, i).is_ascii_digit() && at(b, j).is_ascii_digit() {
            if first_diff == 0 {
                first_diff = at(a, i) as i32 - at(b, j) as i32;
            }
            i += 1;
            j += 1;
        }
        if at(a, i).is_ascii_digit() {
            return 1;
        }
        if at(b, j).is_ascii_digit() {
            return -1;
        }
        if first_diff != 0 {
            return first_diff;
        }
    }
    0
}

impl Ord for Version {
    fn cmp(&self, o: &Self) -> Ordering {
        match self.epoch.cmp(&o.epoch) {
            Ordering::Equal => {}
            x => return x,
        }
        let r = verrevcmp(&self.upstream, &o.upstream);
        if r != 0 {
            return r.cmp(&0);
        }
        verrevcmp(&self.revision, &o.revision).cmp(&0)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl PartialEq for Version {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}

impl Eq for Version {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Op::Lt => "<<",
            Op::Le => "<=",
            Op::Eq => "=",
            Op::Ge => ">=",
            Op::Gt => ">>",
        })
    }
}

pub fn relate(a: &Version, op: Op, b: &Version) -> bool {
    let c = a.cmp(b);
    match op {
        Op::Lt => c == Ordering::Less,
        Op::Le => c != Ordering::Greater,
        Op::Eq => c == Ordering::Equal,
        Op::Ge => c != Ordering::Less,
        Op::Gt => c == Ordering::Greater,
    }
}
