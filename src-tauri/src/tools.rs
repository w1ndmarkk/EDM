//! Installs/updates yt-dlp and deno into the data folder's bin directory (YouTube changes break old
//! yt-dlp builds often). Both are verified against the SHA-256 sums published with the release.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use tokio_util::sync::CancellationToken;

use crate::i18n::t;
use crate::paths::bin_dir;
use crate::segmented::{self, Event, CLIENT};
use crate::store::DEFAULT_USER_AGENT;
use crate::{tf, video};

const YTDLP_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe";
const YTDLP_SUMS_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS";
const DENO_URL: &str = "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip";
/// Approximate download size, shown in the install prompt.
pub const DENO_DOWNLOAD_SIZE: &str = "~45 MB";

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsStatus {
    pub busy: bool,
    pub progress: f64,
    pub message: Option<String>,
    pub ytdlp_path: Option<String>,
    pub ytdlp_version: Option<String>,
    pub deno_path: Option<String>,
    pub deno_version: Option<String>,
    pub ffmpeg_path: Option<String>,
}

pub struct Tools {
    app: AppHandle,
    state: Mutex<ToolsStatus>,
    versions: Mutex<HashMap<PathBuf, String>>,
}

impl Tools {
    pub fn new(app: AppHandle) -> Self {
        Self { app, state: Mutex::new(ToolsStatus::default()), versions: Mutex::new(HashMap::new()) }
    }

    pub fn status(&self) -> ToolsStatus {
        self.state.lock().unwrap().clone()
    }

    fn set(&self, f: impl FnOnce(&mut ToolsStatus)) {
        let snapshot = {
            let mut s = self.state.lock().unwrap();
            f(&mut s);
            s.clone()
        };
        let _ = self.app.emit("tools", snapshot);
    }

    fn version(&self, path: &Path) -> Option<String> {
        if let Some(v) = self.versions.lock().unwrap().get(path) {
            return Some(v.clone());
        }
        let v = video::tool_version(path)?;
        self.versions.lock().unwrap().insert(path.to_path_buf(), v.clone());
        Some(v)
    }

    /// Locates the tools and reads their versions (runs `--version`, so off the async threads).
    pub async fn refresh(self: &Arc<Self>, custom_ytdlp: String) {
        let me = self.clone();
        let found = tokio::task::spawn_blocking(move || {
            let ytdlp = video::locate_ytdlp(&custom_ytdlp);
            let deno = video::locate_deno();
            let ytdlp_version = ytdlp.as_deref().and_then(|p| me.version(p));
            let deno_version = deno.as_deref().and_then(|p| me.version(p));
            (ytdlp, ytdlp_version, deno, deno_version, video::locate_ffmpeg())
        })
        .await;
        if let Ok((ytdlp, yv, deno, dv, ffmpeg)) = found {
            let s = |p: Option<PathBuf>| p.map(|p| p.to_string_lossy().into_owned());
            self.set(|st| {
                st.ytdlp_path = s(ytdlp);
                st.ytdlp_version = yv;
                st.deno_path = s(deno);
                st.deno_version = dv;
                st.ffmpeg_path = s(ffmpeg);
            });
        }
    }

    fn begin(&self, message: String) -> Result<(), String> {
        let mut s = self.state.lock().unwrap();
        if s.busy {
            return Err(t("Another tool install is in progress."));
        }
        s.busy = true;
        s.progress = 0.0;
        s.message = Some(message);
        let snapshot = s.clone();
        drop(s);
        let _ = self.app.emit("tools", snapshot);
        Ok(())
    }

    fn finish(&self, result: &Result<String, String>, success_message: impl FnOnce(&str) -> String) {
        self.set(|s| {
            s.busy = false;
            s.progress = 0.0;
            s.message = Some(match result {
                Ok(v) => success_message(v),
                Err(e) => tf!("Install failed: {0}", e),
            });
        });
    }

    /// Downloads with the app's own segmented downloader, reporting progress to the Settings UI.
    async fn download(self: &Arc<Self>, url: &str, dest: &Path) -> Result<(), String> {
        std::fs::create_dir_all(bin_dir()).map_err(|e| e.to_string())?;
        let url = reqwest::Url::parse(url).map_err(|e| e.to_string())?;
        let cfg = segmented::Config::new(url, vec![("User-Agent".into(), DEFAULT_USER_AGENT.into())], dest.to_path_buf(), 4);
        let total = Arc::new(AtomicI64::new(-1));
        let me = self.clone();
        segmented::download(cfg, CancellationToken::new(), move |ev| match ev {
            Event::Probe(p) => total.store(p.total_bytes, Ordering::Relaxed),
            Event::Progress(received, _) => {
                let t = total.load(Ordering::Relaxed);
                if t > 0 {
                    me.set(|s| s.progress = received as f64 / t as f64);
                }
            }
        })
        .await
        .map_err(|e| e.to_string())
    }

    pub async fn install_ytdlp(self: &Arc<Self>) -> Result<String, String> {
        self.begin(t("Downloading yt-dlp…"))?;
        let result = self.install_ytdlp_inner().await;
        self.finish(&result, |v| tf!("yt-dlp {0} installed.", v));
        result
    }

    async fn install_ytdlp_inner(self: &Arc<Self>) -> Result<String, String> {
        let tmp = bin_dir().join("yt-dlp.new.exe");
        let dest = bin_dir().join("yt-dlp.exe");
        let result = async {
            self.download(YTDLP_URL, &tmp).await?;
            self.set(|s| s.message = Some(t("Verifying…")));
            let sums = fetch_text(YTDLP_SUMS_URL).await?;
            let expected = sums
                .lines()
                .find_map(|l| {
                    let mut parts = l.split_whitespace();
                    let hash = parts.next()?;
                    (parts.next()? == "yt-dlp.exe").then(|| hash.to_lowercase())
                })
                .ok_or_else(|| t("Could not fetch the checksum for verification."))?;
            verify_sha256(&tmp, &expected)?;
            let tmp2 = tmp.clone();
            let version = tokio::task::spawn_blocking(move || video::tool_version(&tmp2))
                .await
                .ok()
                .flatten()
                .ok_or_else(|| t("The downloaded yt-dlp binary could not be executed."))?;
            std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
            self.versions.lock().unwrap().insert(dest.clone(), version.clone());
            Ok(version)
        }
        .await;
        let _ = std::fs::remove_file(&tmp);
        result
    }

    pub async fn install_deno(self: &Arc<Self>) -> Result<String, String> {
        self.begin(t("Downloading deno…"))?;
        let result = self.install_deno_inner().await;
        self.finish(&result, |v| tf!("{0} installed.", v));
        result
    }

    async fn install_deno_inner(self: &Arc<Self>) -> Result<String, String> {
        let zip_path = bin_dir().join("deno.download.zip");
        let tmp = bin_dir().join("deno.new.exe");
        let dest = bin_dir().join("deno.exe");
        let result = async {
            self.download(DENO_URL, &zip_path).await?;
            self.set(|s| s.message = Some(t("Verifying…")));
            // The published file holds the hash somewhere in it (plain "<hash>  <name>" or a Get-FileHash table).
            let sums = fetch_text(&format!("{DENO_URL}.sha256sum")).await?;
            let expected = sums
                .split_whitespace()
                .find(|w| w.len() == 64 && w.chars().all(|c| c.is_ascii_hexdigit()))
                .map(str::to_lowercase)
                .ok_or_else(|| t("Could not fetch the checksum for verification."))?;
            verify_sha256(&zip_path, &expected)?;
            extract_file(&zip_path, "deno.exe", &tmp).map_err(|_| t("The deno archive could not be unpacked."))?;
            let tmp2 = tmp.clone();
            let version = tokio::task::spawn_blocking(move || video::tool_version(&tmp2))
                .await
                .ok()
                .flatten()
                .filter(|v| v.starts_with("deno "))
                .ok_or_else(|| t("The downloaded deno binary could not be executed."))?;
            std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
            self.versions.lock().unwrap().insert(dest.clone(), version.clone());
            Ok(version)
        }
        .await;
        let _ = std::fs::remove_file(&zip_path);
        let _ = std::fs::remove_file(&tmp);
        result
    }
}

async fn fetch_text(url: &str) -> Result<String, String> {
    let resp = CLIENT
        .get(url)
        .header("User-Agent", DEFAULT_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| segmented::error_text(&e))?;
    if !resp.status().is_success() {
        return Err(t("Could not fetch the checksum for verification."));
    }
    resp.text().await.map_err(|e| e.to_string())
}

fn verify_sha256(file: &Path, expected: &str) -> Result<(), String> {
    let mut f = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if actual == expected {
        Ok(())
    } else {
        Err(t("Checksum mismatch; the download was discarded."))
    }
}

fn extract_file(zip_path: &Path, name: &str, dest: &Path) -> std::io::Result<()> {
    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file).map_err(std::io::Error::other)?;
    let mut entry = archive.by_name(name).map_err(std::io::Error::other)?;
    let mut out = std::fs::File::create(dest)?;
    std::io::copy(&mut entry, &mut out)?;
    Ok(())
}
