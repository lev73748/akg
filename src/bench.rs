// Бенчмарк зеркал: akg bench. Запускается только вручную, из update/install не вызывается.
// Стадия 1: пинг всех зеркал одновременно (InRelease).
// Стадия 2: скорость скачивания на ограниченном числе потоков, иначе зеркала делят
// один канал и цифры нельзя сравнивать.
// После таблицы просит выбрать номер: зеркало записывается как основное (и TUR).

use std::cmp::Ordering;
use std::path::Path;
use std::time::{Duration, Instant};

use futures::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;

use crate::progress;
use crate::repo;
use crate::reposet::{self, RepoSet};
use crate::util::*;

const PING_TIMEOUT: u64 = 8;
const SPEED_BYTES: u64 = 4 * 1024 * 1024;
const SPEED_SECS: u64 = 5;
const SPEED_PAR: usize = 3;

struct Mirror {
    url: String,
    host: String,
    suite: String,
    comp: String,
    current: bool,
    set: RepoSet,
}

struct Row {
    host: String,
    current: bool,
    ping: Option<Duration>,
    speed: Option<f64>,
    err: Option<String>,
    set: RepoSet,
}

fn mirror_of(set: RepoSet, current: bool) -> Mirror {
    let main = set.members[0].clone();
    let url = main.uri.trim_end_matches('/').to_string();
    Mirror {
        host: reposet::host_of(&url),
        url,
        suite: main.suite.clone(),
        comp: main.comps.first().cloned().unwrap_or_else(|| "main".to_string()),
        current,
        set,
    }
}

fn collect(prefix: &Path) -> R<Vec<Mirror>> {
    let mut list: Vec<Mirror> = Vec::new();

    if repo::is_custom() {
        for s in repo::read_sources(prefix) {
            let set = RepoSet {
                name: reposet::display_name(&s.uri),
                members: vec![s],
                user: false,
            };
            list.push(mirror_of(set, false));
        }
    } else {
        let cur = reposet::current_main(prefix);
        let mut sets = reposet::mirror_sets(prefix);
        if let Some(c) = &cur {
            if !sets.iter().any(|s| s.members[0].uri == *c) {
                sets.push(reposet::single_main(c));
            }
        }
        for set in sets {
            let is_cur = cur.as_deref() == Some(set.members[0].uri.as_str());
            list.push(mirror_of(set, is_cur));
        }
    }

    if list.is_empty() {
        return Err("no mirrors found (etc/termux/mirrors is empty, no sources configured)".into());
    }
    Ok(list)
}

fn rt() -> R<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

fn client(total_secs: u64) -> R<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(UA)
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(total_secs))
        .build()
        .map_err(|e| e.to_string())
}

fn short(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timeout".to_string()
    } else if e.is_connect() {
        "connection failed".to_string()
    } else {
        "request failed".to_string()
    }
}

// Время от отправки запроса до заголовков ответа (DNS + TCP + TLS + первый байт).
async fn ping(c: &reqwest::Client, m: &Mirror) -> R<Duration> {
    let url = format!("{}/dists/{}/InRelease", m.url, m.suite);
    let t = Instant::now();
    let resp = c.get(&url).send().await.map_err(|e| short(&e))?;
    let d = t.elapsed();
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let _ = resp.bytes().await;
    Ok(d)
}

// Байт/с по первым SPEED_BYTES индекса пакетов (или за SPEED_SECS секунд). Время считается
// после получения заголовков, рукопожатие в скорость не входит.
async fn speed(c: &reqwest::Client, m: &Mirror, arch_name: &str) -> R<f64> {
    let mut last = String::from("no index found");
    for name in ["Packages", "Packages.xz", "Packages.gz"] {
        let url = format!(
            "{}/dists/{}/{}/binary-{}/{}",
            m.url, m.suite, m.comp, arch_name, name
        );
        let resp = match c
            .get(&url)
            .header("Range", format!("bytes=0-{}", SPEED_BYTES - 1))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Err(short(&e)),
        };
        if !resp.status().is_success() {
            last = format!("HTTP {}", resp.status().as_u16());
            continue;
        }
        let t0 = Instant::now();
        let limit = Duration::from_secs(SPEED_SECS);
        let mut got = 0u64;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                break;
            };
            got += chunk.len() as u64;
            if got >= SPEED_BYTES || t0.elapsed() >= limit {
                break;
            }
        }
        let secs = t0.elapsed().as_secs_f64().max(0.001);
        if got == 0 {
            return Err("empty response".to_string());
        }
        return Ok(got as f64 / secs);
    }
    Err(last)
}

fn bar(len: usize, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(len as u64);
    let tpl = format!("{label} {{bar:30.cyan/blue}} {{pos}}/{{len}} mirrors checked");
    pb.set_style(
        ProgressStyle::with_template(&tpl)
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("━╸━"),
    );
    pb
}

fn pool(n: usize) -> R<rayon::ThreadPool> {
    rayon::ThreadPoolBuilder::new()
        .num_threads(n.max(1))
        .build()
        .map_err(|e| e.to_string())
}

fn probe_ping(m: &Mirror) -> R<Duration> {
    let c = client(PING_TIMEOUT)?;
    let rt = rt()?;
    let r = rt.block_on(ping(&c, m));
    r
}

fn probe_speed(m: &Mirror, arch_name: &str) -> R<f64> {
    let c = client(SPEED_SECS + 6)?;
    let rt = rt()?;
    let r = rt.block_on(speed(&c, m, arch_name));
    r
}

fn ping_all(list: &[Mirror]) -> R<Vec<R<Duration>>> {
    let pb = bar(list.len(), "Ping ");
    let p = pool(list.len().min(32))?;
    let out: Vec<R<Duration>> = p.install(|| {
        list.par_iter()
            .map(|m| {
                let r = probe_ping(m);
                pb.inc(1);
                r
            })
            .collect()
    });
    pb.finish_and_clear();
    Ok(out)
}

fn speed_all(list: &[Mirror], idx: &[usize], arch_name: &str) -> R<Vec<(usize, R<f64>)>> {
    let pb = bar(idx.len(), "Speed");
    let p = pool(SPEED_PAR.min(idx.len()))?;
    let out: Vec<(usize, R<f64>)> = p.install(|| {
        idx.par_iter()
            .map(|&i| {
                let r = probe_speed(&list[i], arch_name);
                pb.inc(1);
                (i, r)
            })
            .collect()
    });
    pb.finish_and_clear();
    Ok(out)
}

fn group(r: &Row) -> u8 {
    if r.speed.is_some() {
        0
    } else if r.ping.is_some() {
        1
    } else {
        2
    }
}

pub fn run(prefix: &Path, yes: bool) -> R<()> {
    let t0 = Instant::now();
    let list = collect(prefix)?;
    let arch_name = arch();
    eprintln!(
        "{} {} mirrors, arch {}",
        paint("36", "akg: benchmarking"),
        list.len(),
        arch_name
    );

    let pings = ping_all(&list)?;
    let ok: Vec<usize> = (0..list.len()).filter(|&i| pings[i].is_ok()).collect();

    let mut speeds: Vec<Option<R<f64>>> = (0..list.len()).map(|_| None).collect();
    if !ok.is_empty() {
        for (i, r) in speed_all(&list, &ok, &arch_name)? {
            speeds[i] = Some(r);
        }
    }

    let mut rows: Vec<Row> = Vec::new();
    for (i, m) in list.into_iter().enumerate() {
        let (ping, mut err) = match &pings[i] {
            Ok(d) => (Some(*d), None),
            Err(e) => (None, Some(e.clone())),
        };
        let mut speed = None;
        match speeds[i].take() {
            Some(Ok(v)) => speed = Some(v),
            Some(Err(e)) => err = Some(e),
            None => {}
        }
        rows.push(Row {
            host: m.host,
            current: m.current,
            ping,
            speed,
            err,
            set: m.set,
        });
    }

    rows.sort_by(|a, b| {
        let ga = group(a);
        let gb = group(b);
        ga.cmp(&gb).then_with(|| match ga {
            0 => b.speed.partial_cmp(&a.speed).unwrap_or(Ordering::Equal),
            1 => a.ping.cmp(&b.ping),
            _ => a.host.cmp(&b.host),
        })
    });

    let w = rows
        .iter()
        .map(|r| r.host.chars().count())
        .max()
        .unwrap_or(6)
        .max(6);
    println!(
        "{}",
        paint(
            "1;33",
            &format!("{:>3}  {:<w$}  {:>8}  {}", "#", "mirror", "ping", "speed", w = w)
        )
    );
    for (i, r) in rows.iter().enumerate() {
        let ping = match r.ping {
            Some(d) => format!("{} ms", d.as_millis()),
            None => "-".to_string(),
        };
        let last = match (r.speed, &r.err) {
            (Some(s), _) => format!("{}/s", fmt_bytes(s)),
            (None, Some(e)) => paint("31", e),
            (None, None) => "-".to_string(),
        };
        let cur = if r.current { "  (current)" } else { "" };
        println!("{:>3}  {:<w$}  {:>8}  {}{}", i + 1, r.host, ping, last, cur, w = w);
    }

    eprintln!("done in {}", progress::fmt_dur(t0.elapsed()));

    if rows.iter().all(|r| r.ping.is_none()) {
        return Err("no mirror responded".into());
    }
    if repo::is_custom() {
        return Ok(());
    }

    // выбор: номер, Enter = самое быстрое; -y сразу берёт самое быстрое
    println!();
    let chosen: Option<usize> = if yes {
        rows.first().filter(|r| r.speed.is_some()).map(|_| 0)
    } else if reposet::interactive() {
        match reposet::pick(rows.len(), 1, false) {
            reposet::Choice::Index(k) => Some(k - 1),
            _ => {
                println!("cancelled");
                None
            }
        }
    } else {
        println!("fastest: {}", rows[0].host);
        println!("(run akg bench in a terminal to choose a mirror)");
        None
    };

    if let Some(i) = chosen {
        if rows[i].ping.is_none() {
            return Err(format!("{}: did not respond, choose another", rows[i].host));
        }
        reposet::apply(prefix, &rows[i].set)?;
    }
    Ok(())
}
