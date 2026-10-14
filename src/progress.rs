// Прогресс на indicatif (стиль pip). Загрузка и распаковка (Download/Extract) рисуются
// двумя барами, пока работают параллельные задачи; стадии установки (Install/Configure/
// Trigger) — одним баром на стадию. Вывод скриптов идёт через emit() и печатается над баром.
// Если stderr не терминал, ничего не рисуется (печатаются обычные строки).

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

pub fn is_tty() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stderr().is_terminal())
}

pub fn width() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(2, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        60
    }
}

pub fn fmt_secs(s: f64) -> String {
    let s = if s.is_finite() { s.max(0.0) as u64 } else { 0 };
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}s", s)
    }
}

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 10.0 {
        format!("{:.1}s", s)
    } else {
        fmt_secs(s)
    }
}

// ------------------------------------------------------------ стили

fn mk_style(tpl: &str) -> ProgressStyle {
    ProgressStyle::with_template(tpl)
        .unwrap_or_else(|_| ProgressStyle::default_bar())
        .progress_chars("━╸━")
}

fn dl_style(narrow: bool) -> ProgressStyle {
    if narrow {
        mk_style("{prefix:<10} {wide_bar:.green/238} {percent:>3}% {bytes_per_sec}")
    } else {
        mk_style(
            "{prefix:<10} {wide_bar:.green/238} {bytes}/{total_bytes} {bytes_per_sec} eta {eta_precise} {msg}",
        )
    }
}

fn ex_style() -> ProgressStyle {
    mk_style("{prefix:<10} {wide_bar:.cyan/238} {msg}")
}

fn step_style(narrow: bool) -> ProgressStyle {
    if narrow {
        mk_style("{prefix:<10} {wide_bar:.yellow/238} {pos}/{len} {msg}")
    } else {
        mk_style("{prefix:<10} {wide_bar:.yellow/238} {pos}/{len} {elapsed_precise} {msg}")
    }
}

// ------------------------------------------------------------ стадии (один бар на стадию)

struct Phase {
    label: String,
    pb: ProgressBar,
    cur: String,
    t0: Instant,
}

static PHASE: Mutex<Option<Phase>> = Mutex::new(None);

fn phase_lock() -> MutexGuard<'static, Option<Phase>> {
    PHASE.lock().unwrap_or_else(|e| e.into_inner())
}

// если шаг шёл долго, напишем сколько
fn note_slow(p: &Phase) {
    let d = p.t0.elapsed();
    if !p.cur.is_empty() && d >= Duration::from_secs(5) {
        p.pb.println(format!("  {}: {} took {}", p.label, p.cur, fmt_dur(d)));
    }
}

fn finish_phase_bar(pb: &ProgressBar) {
    let full = pb.length().map_or(false, |l| pb.position() >= l);
    if full {
        pb.finish_with_message("");
    } else {
        pb.abandon();
    }
}

pub fn bar_active() -> bool {
    is_tty() && phase_lock().is_some()
}

/// Показать шаг стадии: done — сколько шагов уже сделано до этого, n — всего.
/// Бар и номер считают текущий шаг (на последнем шаге 12/12 бар полный).
pub fn step(label: &str, done: usize, n: usize, name: &str) {
    let label = label.trim();
    // в коде установки стадия называется Unpack, на экране — Install (Extract занят загрузкой)
    let label = if label == "Unpack" { "Install" } else { label };
    if !is_tty() {
        eprintln!("  [{}/{}] {} {}", done + 1, n, label, name);
        return;
    }
    let mut g = phase_lock();
    let reuse = matches!(&*g, Some(p) if p.label == label && p.pb.length() == Some(n as u64));
    if !reuse {
        if let Some(old) = g.take() {
            note_slow(&old);
            finish_phase_bar(&old.pb);
        }
        let pb = ProgressBar::with_draw_target(Some(n as u64), ProgressDrawTarget::stderr_with_hz(10));
        pb.set_style(step_style(width() < 70));
        pb.set_prefix(label.to_string());
        pb.enable_steady_tick(Duration::from_millis(100));
        *g = Some(Phase { label: label.to_string(), pb, cur: String::new(), t0: Instant::now() });
    }
    if let Some(p) = g.as_mut() {
        note_slow(p);
        p.pb.set_position((done + 1) as u64);
        p.pb.set_message(name.to_string());
        p.cur = name.to_string();
        p.t0 = Instant::now();
    }
}

/// Напечатать строку над баром (или просто в stderr, если бара нет).
pub fn emit(text: &str) {
    let text = text.trim_end_matches(|c: char| c == '\n' || c == '\r');
    let g = phase_lock();
    match &*g {
        Some(p) if !p.pb.is_hidden() => p.pb.println(text),
        _ => eprintln!("{text}"),
    }
}

/// Закончить текущую стадию: полный бар остаётся на экране, незавершённый — как есть.
pub fn clear_line() {
    let mut g = phase_lock();
    if let Some(p) = g.take() {
        note_slow(&p);
        finish_phase_bar(&p.pb);
    }
}

// ------------------------------------------------------------ загрузка + распаковка

pub struct Shared {
    pub dl_pkgs: usize,
    pub ex_pkgs: usize,
    pub dl_total: u64,
    pub ex_total: u64,
    pub dl_bytes: Arc<AtomicU64>,
    pub dl_done: AtomicUsize,
    pub ex_bytes: AtomicU64,
    pub ex_done: AtomicUsize,
    stop: AtomicBool,
}

impl Shared {
    /// dl_pkgs = 0 скрывает бар загрузки, ex_pkgs = 0 скрывает бар распаковки.
    pub fn new(dl_pkgs: usize, ex_pkgs: usize, dl_total: u64, ex_total: u64) -> Arc<Shared> {
        Arc::new(Shared {
            dl_pkgs,
            ex_pkgs,
            dl_total,
            ex_total,
            dl_bytes: Arc::new(AtomicU64::new(0)),
            dl_done: AtomicUsize::new(0),
            ex_bytes: AtomicU64::new(0),
            ex_done: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
        })
    }
}

pub struct Renderer {
    sh: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Renderer {
    pub fn start(sh: Arc<Shared>) -> Renderer {
        if !is_tty() {
            return Renderer { sh, handle: None };
        }
        let s = sh.clone();
        let handle = thread::spawn(move || {
            let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr_with_hz(10));
            let narrow = width() < 70;

            let dl = if s.dl_pkgs > 0 {
                let pb = mp.add(ProgressBar::new(s.dl_total.max(1)));
                pb.set_style(dl_style(narrow));
                pb.set_prefix("Download");
                Some(pb)
            } else {
                None
            };
            let ex = if s.ex_pkgs > 0 {
                let pb = mp.add(ProgressBar::new(s.ex_total.max(1)));
                pb.set_style(ex_style());
                pb.set_prefix("Extract");
                Some(pb)
            } else {
                None
            };

            loop {
                let stopping = s.stop.load(Ordering::Relaxed);

                let mut dl_ok = true;
                if let Some(pb) = &dl {
                    let done = s.dl_done.load(Ordering::Relaxed).min(s.dl_pkgs);
                    dl_ok = done >= s.dl_pkgs;
                    let pos = if dl_ok {
                        s.dl_total.max(1)
                    } else {
                        s.dl_bytes.load(Ordering::Relaxed).min(s.dl_total)
                    };
                    pb.set_position(pos);
                    pb.set_message(format!("{}/{}", done, s.dl_pkgs));
                }

                let mut ex_ok = true;
                if let Some(pb) = &ex {
                    let done = s.ex_done.load(Ordering::Relaxed).min(s.ex_pkgs);
                    ex_ok = done >= s.ex_pkgs;
                    let pos = if ex_ok {
                        s.ex_total.max(1)
                    } else {
                        s.ex_bytes.load(Ordering::Relaxed).min(s.ex_total)
                    };
                    pb.set_position(pos);
                    pb.set_message(format!("{}/{} packages", done, s.ex_pkgs));
                }

                if stopping {
                    if let Some(pb) = &dl {
                        if dl_ok {
                            pb.finish();
                        } else {
                            pb.abandon();
                        }
                    }
                    if let Some(pb) = &ex {
                        if ex_ok {
                            pb.finish();
                        } else {
                            pb.abandon();
                        }
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
        Renderer { sh, handle: Some(handle) }
    }

    /// Остановить и дорисовать последний кадр. Можно вызывать несколько раз.
    pub fn finish(&mut self) {
        self.sh.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.finish();
    }
}
