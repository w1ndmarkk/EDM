//! In-app sign-in window (WebView2). When it is closed (or "Save Cookies" is pressed in Settings) the session
//! cookies are exported to a Netscape cookie file that yt-dlp uses for age-restricted / members-only videos.

use std::path::PathBuf;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::i18n::t;

const LABEL: &str = "login";
const START_URL: &str = "https://accounts.google.com/ServiceLogin?continue=https://www.youtube.com/";

pub fn cookie_file() -> PathBuf {
    crate::paths::cookies_dir().join("sites.txt")
}

/// Path of the shared cookie file if it exists.
pub fn cookie_file_path() -> Option<PathBuf> {
    Some(cookie_file()).filter(|p| p.is_file())
}

fn rows() -> Vec<String> {
    std::fs::read_to_string(cookie_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Registrable-ish domains (last two labels) the saved cookies belong to.
pub fn domains() -> Vec<String> {
    let mut out: Vec<String> = rows()
        .iter()
        .filter_map(|r| r.split('\t').next())
        .map(|h| {
            let h = h.trim_start_matches('.');
            let parts: Vec<&str> = h.split('.').collect();
            if parts.len() >= 2 { parts[parts.len() - 2..].join(".") } else { h.to_string() }
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LoginStatus {
    pub cookie_count: usize,
    pub saved_at: Option<i64>,
    pub domains: Vec<String>,
    pub window_open: bool,
}

pub fn status(app: &AppHandle) -> LoginStatus {
    let saved_at = std::fs::metadata(cookie_file())
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64);
    LoginStatus { cookie_count: rows().len(), saved_at, domains: domains(), window_open: app.get_webview_window(LABEL).is_some() }
}

fn broadcast(app: &AppHandle) {
    let _ = app.emit("login", status(app));
}

pub async fn open(app: AppHandle) {
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.unminimize();
        let _ = w.set_focus();
        return;
    }
    let Ok(url) = tauri::Url::parse(START_URL) else { return };
    let built = WebviewWindowBuilder::new(&app, LABEL, WebviewUrl::External(url))
        .title(t("Site Login — sign in, then close this window to save the cookies"))
        .inner_size(1000.0, 720.0)
        .center()
        .build();
    match built {
        Ok(w) => {
            let handle = app.clone();
            w.on_window_event(move |e| {
                if let WindowEvent::CloseRequested { api, .. } = e {
                    // Read the cookies before the webview goes away (not on this thread: WebView2 would deadlock).
                    api.prevent_close();
                    let handle = handle.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = save(&handle).await;
                        if let Some(w) = handle.get_webview_window(LABEL) {
                            let _ = w.destroy();
                        }
                        broadcast(&handle);
                    });
                }
            });
            let _ = w.set_focus();
            broadcast(&app);
        }
        Err(e) => eprintln!("login window: {e}"),
    }
}

/// Exports all cookies of the sign-in window's profile. Returns the number of cookies written.
pub async fn save(app: &AppHandle) -> Result<usize, String> {
    let w = app.get_webview_window(LABEL).ok_or_else(|| t("The sign-in window is not open."))?;
    let cookies = w.cookies().map_err(|e| e.to_string())?;
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string(), "# Exported by EDM".to_string()];
    for c in &cookies {
        let domain = c.domain().unwrap_or_default();
        if domain.is_empty() {
            continue;
        }
        let flag = if domain.starts_with('.') { "TRUE" } else { "FALSE" };
        let secure = if c.secure().unwrap_or(false) { "TRUE" } else { "FALSE" };
        let exp = c.expires_datetime().map(|d| d.unix_timestamp()).unwrap_or(0);
        lines.push([domain, flag, c.path().unwrap_or("/"), secure, &exp.to_string(), c.name(), c.value()].join("\t"));
    }
    let path = cookie_file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, lines.join("\n") + "\n").map_err(|e| e.to_string())?;
    broadcast(app);
    Ok(lines.len() - 2)
}

pub fn clear(app: &AppHandle) {
    let _ = std::fs::remove_file(cookie_file());
    // All app webviews share one WebView2 profile, so clearing any of them signs the login window out too.
    let target = app.get_webview_window(LABEL).or_else(|| app.webview_windows().into_values().next());
    if let Some(w) = target {
        let _ = w.clear_all_browsing_data();
    }
    broadcast(app);
}
