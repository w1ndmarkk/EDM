//! Checks GitHub Releases for a newer Windows build and offers to download its installer.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};

use crate::i18n::t;
use crate::prompt::{self, PromptSpec};
use crate::segmented::{error_text, CLIENT};
use crate::server::VERSION;
use crate::settings::SettingsStore;
use crate::store::{NewDownload, Store};
use crate::{tf, util};

/// GitHub "owner/repo" whose releases carry the Windows installers. None turns the update checker off entirely
/// (no background checks, no tray item, no Settings section) until a repository is set here.
pub const REPO: Option<&str> = Some("w1ndmarkk/EDM");

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdaterStatus {
    pub enabled: bool,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub checking: bool,
    pub last_check: Option<i64>,
    pub last_error: Option<String>,
}

struct Release {
    version: String,
    page: String,
    /// Windows installer asset (setup .exe preferred over .msi).
    installer: Option<(String, String)>,
    notes: String,
}

pub struct Updater {
    app: AppHandle,
    store: Arc<Store>,
    settings: Arc<SettingsStore>,
    state: Mutex<UpdaterStatus>,
}

impl Updater {
    pub fn new(app: AppHandle, store: Arc<Store>, settings: Arc<SettingsStore>) -> Arc<Self> {
        let state = UpdaterStatus { enabled: REPO.is_some(), current_version: VERSION.into(), ..Default::default() };
        Arc::new(Self { app, store, settings, state: Mutex::new(state) })
    }

    pub fn status(&self) -> UpdaterStatus {
        self.state.lock().unwrap().clone()
    }

    fn set(&self, f: impl FnOnce(&mut UpdaterStatus)) {
        let s = {
            let mut st = self.state.lock().unwrap();
            f(&mut st);
            st.clone()
        };
        let _ = self.app.emit("updater", s);
    }

    pub fn start(self: &Arc<Self>) {
        if REPO.is_none() {
            return;
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_secs(8)).await;
            loop {
                let s = me.settings.get();
                if s.check_for_updates && util::now_ms() - s.last_update_check > 20 * 3600 * 1000 {
                    me.check(false).await;
                }
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
    }

    pub async fn check(self: &Arc<Self>, interactive: bool) {
        let Some(repo) = REPO else { return };
        {
            let mut st = self.state.lock().unwrap();
            if st.checking {
                return;
            }
            st.checking = true;
            st.last_error = None;
        }
        self.set(|_| {});
        let result = fetch_latest(repo).await;
        let now = util::now_ms();
        self.settings.update(&self.app, |s| s.last_update_check = now);
        match result {
            Err(e) => {
                self.set(|s| {
                    s.checking = false;
                    s.last_check = Some(now);
                    s.last_error = Some(e.clone());
                });
                if interactive {
                    self.show_no_update(Some(e)).await;
                }
            }
            Ok(release) => {
                self.set(|s| {
                    s.checking = false;
                    s.last_check = Some(now);
                    s.latest_version = Some(release.version.clone());
                });
                if is_newer(&release.version, VERSION) {
                    // A release without a Windows installer is only shown when asked for.
                    let skipped = self.settings.get().skipped_version == release.version;
                    if interactive || (release.installer.is_some() && !skipped) {
                        self.show_update(release).await;
                    }
                } else if interactive {
                    self.show_no_update(None).await;
                }
            }
        }
    }

    async fn show_update(&self, r: Release) {
        let mut message = tf!("You are using version {0}.", VERSION);
        if !r.notes.is_empty() {
            message.push_str("\n\n");
            message.push_str(&r.notes.chars().take(600).collect::<String>());
        }
        let first = if r.installer.is_some() { t("Download Update") } else { t("Open Release Page") };
        let spec = PromptSpec {
            title: tf!("EDM {0} is available", r.version),
            message,
            buttons: vec![first, t("Later"), t("Skip This Version")],
            default_button: 0,
            cancel_button: Some(1),
            ..Default::default()
        };
        match prompt::ask(&self.app, spec).await.button {
            Some(0) => match r.installer {
                Some((name, url)) => {
                    self.store.add(NewDownload { url, file_name: Some(name), ..Default::default() });
                }
                None => util::open_url(&r.page),
            },
            Some(2) => {
                let v = r.version.clone();
                self.settings.update(&self.app, |s| s.skipped_version = v);
            }
            _ => {}
        }
    }

    async fn show_no_update(&self, error: Option<String>) {
        let spec = match error {
            Some(e) => PromptSpec { title: t("Could not check for updates"), message: e, ..Default::default() },
            None => PromptSpec {
                title: t("You're up to date"),
                message: tf!("EDM {0} is the latest version.", VERSION),
                ..Default::default()
            },
        };
        let spec = PromptSpec { buttons: vec!["OK".into()], cancel_button: Some(0), ..spec };
        prompt::ask(&self.app, spec).await;
    }
}

async fn fetch_latest(repo: &str) -> Result<Release, String> {
    let resp = CLIENT
        .get(format!("https://api.github.com/repos/{repo}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("EDM/{VERSION}"))
        .timeout(Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| error_text(&e))?;
    if !resp.status().is_success() {
        return Err(t("No release information available yet."));
    }
    let obj: Value = serde_json::from_str(&resp.text().await.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let tag = obj["tag_name"].as_str().ok_or_else(|| t("No release information available yet."))?;
    let assets: Vec<(String, String)> = obj["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| Some((x["name"].as_str()?.to_string(), x["browser_download_url"].as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let is_x64 = |n: &str| !n.contains("arm64") && !n.contains("aarch64");
    let installer = assets
        .iter()
        .find(|(n, _)| {
            let n = n.to_lowercase();
            n.ends_with(".exe") && n.contains("setup") && is_x64(&n)
        })
        .or_else(|| assets.iter().find(|(n, _)| n.to_lowercase().ends_with(".msi") && is_x64(&n.to_lowercase())))
        .cloned();
    Ok(Release {
        version: tag.trim_start_matches('v').to_string(),
        page: obj["html_url"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://github.com/{repo}/releases/latest")),
        installer,
        notes: obj["body"].as_str().unwrap_or("").trim().to_string(),
    })
}

pub fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.').map(|p| p.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)).collect()
    };
    let (pa, pb) = (parse(a), parse(b));
    for i in 0..pa.len().max(pb.len()) {
        let (x, y) = (pa.get(i).copied().unwrap_or(0), pb.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::is_newer("1.2.0", "1.1.9"));
        assert!(super::is_newer("2.0", "1.9.9"));
        assert!(!super::is_newer("1.1.1", "1.1.1"));
        assert!(!super::is_newer("1.1", "1.1.1"));
    }
}
