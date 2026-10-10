// Загрузчик на tokio + reqwest. Только HTTP/HTTPS.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures::StreamExt;
use sha2::{Digest, Sha256};

use crate::util::*;

pub struct Job {
    pub url: String,
    pub dest: PathBuf,
    pub size: u64,
    pub sha256: Option<String>,
}

static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn client() -> &'static reqwest::Client {
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(UA)
            .connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(180))
            .pool_max_idle_per_host(64)
            .build()
            .expect("failed to build reqwest client")
    })
}

async fn fetch_http(job: &Job, bytes: &AtomicU64) -> Result<(), String> {
    let resp = client()
        .get(&job.url)
        .send()
        .await
        .map_err(|e| format!("{}: {e}", job.url))?;

    if !resp.status().is_success() {
        return Err(format!("{}: HTTP {}", job.url, resp.status()));
    }

    if let Some(p) = job.dest.parent() {
        let _ = fs::create_dir_all(p);
    }
    let part = with_suffix(&job.dest, ".akg-part");
    let mut f = File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut h = Sha256::new();
    let mut got = 0u64;

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        f.write_all(&chunk).map_err(|e| e.to_string())?;
        h.update(&chunk);
        got += chunk.len() as u64;
        bytes.fetch_add(chunk.len() as u64, Ordering::Relaxed);
    }

    if job.size > 0 && got != job.size {
        let _ = fs::remove_file(&part);
        bytes.fetch_sub(got, Ordering::Relaxed);
        return Err(format!("size mismatch ({got} != {})", job.size));
    }
    if let Some(exp) = &job.sha256 {
        let d = h.finalize();
        if &hex(d.as_slice()) != exp {
            let _ = fs::remove_file(&part);
            bytes.fetch_sub(got, Ordering::Relaxed);
            return Err("sha256 mismatch".into());
        }
    }
    drop(f);
    fs::rename(&part, &job.dest).map_err(|e| e.to_string())
}

pub async fn download_one(job: &Job, bytes: Arc<AtomicU64>) -> Result<(), String> {
    fetch_http(job, &bytes).await
}

/// Скачать один файл по ссылке (для прямых ссылок на .deb), без проверки хеша.
pub fn download_file(url: &str, dest: &Path) -> R<()> {
    let job = Job { url: url.to_string(), dest: dest.to_path_buf(), size: 0, sha256: None };
    let bytes = AtomicU64::new(0);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(fetch_http(&job, &bytes))
}

pub fn fetch_bytes(url: &str) -> R<Vec<u8>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let resp = client()
            .get(url)
            .send()
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("{url}: HTTP {}", resp.status()));
        }
        let b = resp.bytes().await.map_err(|e| format!("{url}: {e}"))?;
        Ok(b.to_vec())
    })
}
