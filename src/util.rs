use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread;

use md5::{Digest, Md5};

pub type R<T> = Result<T, String>;

pub const UA: &str = "Termux-PKG/2.0 akg/0.5";
pub const DEFAULT_PREFIX: &str = "/data/data/com.termux/files/usr";

pub fn paint(code: &str, s: &str) -> String {
    format!("\x1b[{code}m{s}\x1b[0m")
}

pub fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(H[(*x >> 4) as usize] as char);
        s.push(H[(*x & 15) as usize] as char);
    }
    s
}

pub fn fmt_bytes(b: f64) -> String {
    if b >= 1_048_576.0 {
        format!("{:.1} MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else {
        format!("{:.0} B", b)
    }
}

pub fn with_suffix(p: &Path, suf: &str) -> PathBuf {
    let mut s = p.as_os_str().to_os_string();
    s.push(suf);
    PathBuf::from(s)
}

pub fn set_mtime(path: &Path, secs: u64) {
    use std::os::unix::ffi::OsStrExt;
    if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
        let t = libc::timespec {
            tv_sec: secs as libc::time_t,
            tv_nsec: 0,
        };
        let ts = [t, t];
        unsafe {
            libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), 0);
        }
    }
}

pub fn md5_file(p: &Path) -> io::Result<String> {
    let mut f = File::open(p)?;
    let mut h = Md5::new();
    let mut buf = vec![0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(h.finalize().as_slice()))
}

pub fn write_atomic(path: &Path, data: &[u8]) -> R<()> {
    let tmp = with_suffix(path, ".akg-tmp");
    fs::write(&tmp, data).map_err(|e| format!("{}: {e}", tmp.display()))?;
    fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn warn(r: R<()>) {
    if let Err(e) = r {
        eprintln!("{} {}", paint("33", "warning:"), e);
    }
}

pub fn prefix() -> PathBuf {
    PathBuf::from(std::env::var("PREFIX").unwrap_or_else(|_| DEFAULT_PREFIX.into()))
}

pub fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/data/data/com.termux/files/home".into());
    PathBuf::from(home).join(".cache/akg")
}

pub fn default_jobs() -> usize {
    let cpus = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (cpus * 3).clamp(6, 16)
}

pub fn arch() -> String {
    if let Ok(a) = std::env::var("AKG_ARCH") {
        return a;
    }
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    unsafe {
        libc::uname(&mut u);
    }
    let m = unsafe { std::ffi::CStr::from_ptr(u.machine.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    match m.as_str() {
        "aarch64" | "arm64" => "aarch64".to_string(),
        "armv7l" | "armv8l" | "arm" => "arm".to_string(),
        "x86_64" => "x86_64".to_string(),
        "i686" | "i386" => "i686".to_string(),
        other => other.to_string(),
    }
}
