//! Tiny HTTP bridge on 127.0.0.1 for the browser extensions.
//! Only requests from extension origins are accepted (see `extension_request`).

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::i18n::t;
use crate::models::DownloadKind;
use crate::settings::SettingsStore;
use crate::store::{ExtensionCookie, NewDownload, Store};
use crate::{tf, util, video};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_REQUEST: usize = 16_000_000;

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub running: bool,
    pub status_text: String,
    pub port: u16,
    /// Last time each browser's extension talked to the bridge (keys: chrome, edge, brave, other), ms.
    pub connections: HashMap<String, i64>,
}

pub struct Server {
    app: AppHandle,
    store: Arc<Store>,
    settings: Arc<SettingsStore>,
    status: Mutex<ServerStatus>,
    stop: Mutex<Option<CancellationToken>>,
    formats_cache: Mutex<HashMap<String, (std::time::Instant, video::FormatList)>>,
    /// One yt-dlp run per page at a time (the early read and the click that follows it).
    formats_inflight: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl Server {
    pub fn new(app: AppHandle, store: Arc<Store>, settings: Arc<SettingsStore>) -> Arc<Self> {
        Arc::new(Self {
            app,
            store,
            settings,
            status: Mutex::new(ServerStatus { status_text: t("Not started"), ..Default::default() }),
            stop: Mutex::new(None),
            formats_cache: Mutex::new(HashMap::new()),
            formats_inflight: Mutex::new(HashMap::new()),
        })
    }

    pub fn status(&self) -> ServerStatus {
        self.status.lock().unwrap().clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut ServerStatus)) {
        let s = {
            let mut st = self.status.lock().unwrap();
            f(&mut st);
            st.clone()
        };
        let _ = self.app.emit("server", s);
    }

    /// (Re)starts the listener on the configured port.
    pub fn start(self: &Arc<Self>) {
        let token = CancellationToken::new();
        if let Some(old) = self.stop.lock().unwrap().replace(token.clone()) {
            old.cancel();
        }
        let port = self.settings.get().server_port;
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            // Give a previous listener on the same port a moment to close.
            tokio::time::sleep(Duration::from_millis(100)).await;
            let listener = match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => l,
                Err(e) => {
                    me.set_status(|s| {
                        s.running = false;
                        s.port = port;
                        s.status_text = tf!("Error: {0} (port {1} may be in use)", e, port);
                    });
                    return;
                }
            };
            me.set_status(|s| {
                s.running = true;
                s.port = port;
                s.status_text = tf!("Listening on 127.0.0.1:{0}", port);
            });
            loop {
                tokio::select! {
                    _ = token.cancelled() => break,
                    accepted = listener.accept() => {
                        if let Ok((stream, _)) = accepted {
                            let me = me.clone();
                            tokio::spawn(async move { me.serve(stream, port).await });
                        }
                    }
                }
            }
            me.set_status(|s| {
                s.running = false;
                s.status_text = t("Stopped");
            });
        });
    }

    /// 15 s to send the request; the answer may take longer (yt-dlp reading a slow page for /formats can take
    /// a minute or more, and cutting it off made the extension report "EDM is not running").
    async fn serve(&self, mut stream: TcpStream, port: u16) {
        let read = async {
            let mut buf = Vec::with_capacity(4096);
            let mut chunk = [0u8; 65536];
            loop {
                match stream.read(&mut chunk).await {
                    Ok(0) | Err(_) => return None,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                if let Some(r) = parse(&buf) {
                    return Some(r);
                }
                if buf.len() > MAX_REQUEST {
                    return None;
                }
            }
        };
        let Ok(Some(req)) = tokio::time::timeout(Duration::from_secs(15), read).await else { return };
        let origin = req.headers.get("origin").cloned();
        let Ok((status, body)) = tokio::time::timeout(Duration::from_secs(240), self.handle(&req, port)).await else { return };
        let _ = send(&mut stream, status, &body, origin.as_deref()).await;
    }

    fn note_connection(&self, req: &Request) {
        let mut browser = "other".to_string();
        if let Some(origin) = req.headers.get("origin") {
            if origin.starts_with("chrome-extension://") {
                browser = "chrome".into();
            } else if origin.starts_with("moz-extension://") {
                browser = "firefox".into();
            }
        }
        if let Some(h) = req.headers.get("x-edm-extension").map(|h| h.to_lowercase()) {
            if ["chrome", "brave", "edge", "firefox"].contains(&h.as_str()) {
                browser = h;
            }
        }
        if let Some((_, query)) = req.path.split_once('?') {
            for pair in query.split('&') {
                if let Some(("browser", v)) = pair.split_once('=') {
                    browser = v.to_lowercase();
                }
            }
        }
        let now = util::now_ms();
        let previous = self.status.lock().unwrap().connections.get(&browser).copied();
        if previous.is_some_and(|p| now - p < 30_000) {
            self.status.lock().unwrap().connections.insert(browser, now);
        } else {
            self.set_status(|s| {
                s.connections.insert(browser, now);
            });
        }
    }

    async fn handle(&self, req: &Request, port: u16) -> (u16, Value) {
        let host_ok = req.headers.get("host").is_some_and(|h| {
            let h = h.to_lowercase();
            h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}") || h == format!("[::1]:{port}")
        });
        if !extension_request(req) || !host_ok {
            return (403, json!({"ok": false, "error": "forbidden"}));
        }
        if req.method == "OPTIONS" {
            return (200, json!({"ok": true}));
        }
        self.note_connection(req);
        let path = req.path.split('?').next().unwrap_or("");
        let settings = self.settings.get();
        match (req.method.as_str(), path) {
            ("GET", "/ping") => (
                200,
                json!({
                    "ok": true,
                    "app": "EDM",
                    "platform": "windows",
                    "version": VERSION,
                    "ytdlp": video::locate_ytdlp(&settings.ytdlp_path).is_some(),
                    "active": self.store.summary().active_count,
                }),
            ),
            // The same rules for the extension to apply on its own (link clicks, Firefox responses): no round trip,
            // so the browser does not start downloading while it waits.
            ("GET", "/rules") => (200, json!({ "ok": true, "rules": settings.capture_rules() })),
            // Should this browser download be handed to the app? (File types, excluded sites and addresses.)
            ("POST", "/check") => {
                let Ok(body) = serde_json::from_slice::<CheckBody>(&req.body) else {
                    return (400, json!({"ok": false, "error": "invalid request"}));
                };
                let capture = reqwest::Url::parse(&body.url)
                    .ok()
                    .filter(|u| matches!(u.scheme(), "http" | "https"))
                    .is_some_and(|u| settings.should_capture(&u, body.filename.as_deref().filter(|f| !f.is_empty())));
                (200, json!({"ok": true, "capture": capture}))
            }
            // Qualities of the video on a page, for the "Download this video" panel.
            ("POST", "/formats") => {
                let Ok(body) = serde_json::from_slice::<FormatsBody>(&req.body) else {
                    return (400, json!({"ok": false, "error": "invalid request"}));
                };
                match self.formats(body, &settings).await {
                    Ok(list) => (200, json!({"ok": true, "extractor": list.extractor, "title": list.title, "duration": list.duration, "formats": list.options})),
                    Err(e) => (200, json!({"ok": false, "error": e})),
                }
            }
            // "Download playlist" in the page panel: the app's playlist dialog opens with this address.
            ("POST", "/playlist") => {
                let Some(url) = serde_json::from_slice::<Value>(&req.body)
                    .ok()
                    .and_then(|v| v["url"].as_str().map(str::to_string))
                    .filter(|u| reqwest::Url::parse(u).is_ok_and(|u| matches!(u.scheme(), "http" | "https")))
                else {
                    return (400, json!({"ok": false, "error": "invalid url"}));
                };
                crate::windows::show_main(&self.app, Some(format!("playlist:{url}")));
                (200, json!({"ok": true}))
            }
            ("GET", "/downloads") => {
                let items: Vec<Value> = self
                    .store
                    .views()
                    .into_iter()
                    .take(50)
                    .map(|i| {
                        let progress = if i.total_bytes > 0 { (i.received_bytes as f64 / i.total_bytes as f64).min(1.0) } else { 0.0 };
                        json!({
                            "id": i.id, "name": i.file_name, "state": i.state, "progress": progress, "speed": i.speed,
                            "total": i.total_bytes, "received": i.received_bytes, "stage": i.stage, "error": i.error_message,
                            // Bot-checked: the extension gives it back to the browser.
                            "handback": i.handback, "url": i.url,
                        })
                    })
                    .collect();
                (200, json!({"ok": true, "items": items}))
            }
            ("POST", "/download") => {
                let Ok(body) = serde_json::from_slice::<DownloadBody>(&req.body) else {
                    return (400, json!({"ok": false, "error": "invalid url"}));
                };
                let req = NewDownload {
                    url: body.url,
                    file_name: body.filename,
                    referer: body.referer,
                    user_agent: body.user_agent,
                    cookie_header: body.cookie_header,
                    cookies: body.cookies.unwrap_or_default(),
                    kind: if body.kind.as_deref() == Some("video") { DownloadKind::Video } else { DownloadKind::File },
                    extra_headers: body.headers.unwrap_or_default(),
                    segments: body.segments,
                    format: body.format,
                    audio_format: body.audio,
                    title: body.title,
                    manifest: body.manifest,
                    ..Default::default()
                };
                match self.store.add(req) {
                    Some(id) => (200, json!({"ok": true, "id": id})),
                    None => (400, json!({"ok": false, "error": "invalid url"})),
                }
            }
            ("POST", "/action") => {
                let Ok(body) = serde_json::from_slice::<ActionBody>(&req.body) else {
                    return (400, json!({"ok": false, "error": "id and action required"}));
                };
                match body.action.as_str() {
                    "pause" => self.store.pause(body.id),
                    "resume" => self.store.resume(body.id),
                    "cancel" => self.store.cancel(body.id),
                    "remove" => self.store.remove(body.id, false),
                    _ => return (400, json!({"ok": false, "error": "unknown action"})),
                }
                (200, json!({"ok": true}))
            }
            _ => (404, json!({"ok": false, "error": "not found"})),
        }
    }

    /// Runs `yt-dlp -J` for the page. The extension asks as soon as a player shows up, so the list is usually
    /// ready (cached) when the panel opens; a request arriving while the same page is being read waits for that run.
    async fn formats(&self, body: FormatsBody, settings: &crate::settings::Settings) -> Result<video::FormatList, String> {
        const TTL: Duration = Duration::from_secs(30 * 60);
        let key = format!("{}|{}", body.url, settings.video_container);
        let cached = || {
            self.formats_cache.lock().unwrap().get(&key).filter(|(at, _)| at.elapsed() < TTL).map(|(_, list)| list.clone())
        };
        if let Some(list) = cached() {
            return Ok(list);
        }
        let gate = self.formats_inflight.lock().unwrap().entry(key.clone()).or_default().clone();
        let _running = gate.lock().await;
        if let Some(list) = cached() {
            return Ok(list);
        }
        let result = self.read_formats(body, settings).await;
        self.formats_inflight.lock().unwrap().remove(&key);
        let list = result?;
        let mut cache = self.formats_cache.lock().unwrap();
        cache.retain(|_, (at, _)| at.elapsed() < TTL);
        cache.insert(key, (std::time::Instant::now(), list.clone()));
        Ok(list)
    }

    async fn read_formats(&self, body: FormatsBody, settings: &crate::settings::Settings) -> Result<video::FormatList, String> {
        let exe = video::locate_ytdlp(&settings.ytdlp_path).ok_or_else(|| t("yt-dlp not found"))?;
        let mut headers = BTreeMap::new();
        if let Some(ua) = body.user_agent.filter(|s| !s.is_empty()) {
            headers.insert("User-Agent".to_string(), ua);
        }
        if let Some(r) = body.referer.filter(|s| !s.is_empty()) {
            headers.insert("Referer".to_string(), r);
        }
        let cookies = body.cookies.unwrap_or_default();
        let cookie_file = if cookies.is_empty() { None } else { crate::store::write_cookie_file(&cookies, Uuid::new_v4()) };
        let host = reqwest::Url::parse(&body.url).ok().and_then(|u| u.host_str().map(str::to_string));
        let mut result = video::list_formats(&exe, &body.url, &headers, cookie_file.as_deref(), true, &settings.video_container).await;
        // YouTube can reject the signed-in client; the same video usually works without cookies.
        if let Err(e) = &result {
            if video::is_youtube(host.as_deref()) && video::is_signed_in_client_failure(e) {
                result = video::list_formats(&exe, &body.url, &headers, None, false, &settings.video_container).await;
            }
        }
        if let Some(cf) = cookie_file {
            let _ = std::fs::remove_file(cf);
        }
        result
    }
}

#[derive(Deserialize)]
struct CheckBody {
    url: String,
    #[serde(default)]
    filename: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FormatsBody {
    url: String,
    #[serde(default)]
    referer: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    cookies: Option<Vec<ExtensionCookie>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DownloadBody {
    url: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    referer: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    cookie_header: Option<String>,
    #[serde(default)]
    cookies: Option<Vec<ExtensionCookie>>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    segments: Option<u32>,
    /// Video: yt-dlp -f expression picked in the page panel.
    #[serde(default)]
    format: Option<String>,
    /// Video: audio-only download ("m4a", "mp3").
    #[serde(default)]
    audio: Option<String>,
    /// Video: file name (page title) for stream URLs.
    #[serde(default)]
    title: Option<String>,
    /// Video: playlist text of a stream the page played from a blob: URL.
    #[serde(default)]
    manifest: Option<String>,
}

#[derive(Deserialize)]
struct ActionBody {
    id: Uuid,
    action: String,
}

/// Only browser extensions may talk to the bridge. Requests without an Origin, with an empty Origin or with
/// the opaque "null" origin (sandboxed iframes, data: pages) are rejected, so ordinary web pages cannot drive the app.
pub fn origin_allowed(origin: Option<&str>) -> bool {
    match origin {
        Some(o) if !o.is_empty() && o != "null" => {
            o.starts_with("chrome-extension://") || o.starts_with("moz-extension://")
        }
        _ => false,
    }
}

/// Extension requests either carry an extension Origin, or — when Chrome omits the Origin header for GETs covered
/// by host permissions — no Origin at all plus the X-EDM-Extension header. A web page cannot produce the second
/// form: custom headers force a CORS preflight, and the preflight's web Origin is rejected here.
fn extension_request(req: &Request) -> bool {
    let origin = req.headers.get("origin").map(String::as_str);
    if origin_allowed(origin) {
        return true;
    }
    let has_header = req.headers.get("x-edm-extension").is_some_and(|h| !h.is_empty());
    origin.is_none() && has_header
}

fn parse(data: &[u8]) -> Option<Request> {
    let head_end = data.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&data[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_uppercase();
    let path = first.next()?.to_string();
    let mut headers = HashMap::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let length: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let body_start = head_end + 4;
    if data.len() - body_start < length {
        return None;
    }
    Some(Request { method, path, headers, body: data[body_start..body_start + length].to_vec() })
}

async fn send(stream: &mut TcpStream, status: u16, body: &Value, origin: Option<&str>) -> std::io::Result<()> {
    let body = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n",
        body.len()
    );
    // CORS headers only for allowed extension origins; ordinary web pages get none.
    if let Some(o) = origin.filter(|o| origin_allowed(Some(o))) {
        head.push_str(&format!(
            "Access-Control-Allow-Origin: {o}\r\nVary: Origin\r\nAccess-Control-Allow-Headers: Content-Type, X-EDM-Extension\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Private-Network: true\r\nAccess-Control-Allow-Local-Network: true\r\n"
        ));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert!(origin_allowed(Some("chrome-extension://abc")));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("null")));
        assert!(!origin_allowed(None));
    }

    #[test]
    fn parses_requests() {
        let raw = b"POST /download HTTP/1.1\r\nHost: 127.0.0.1:48213\r\nContent-Length: 2\r\n\r\n{}";
        let r = parse(raw).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.path, "/download");
        assert_eq!(r.body, b"{}");
        assert!(parse(b"GET / HTTP/1.1\r\nContent-Length: 5\r\n\r\nab").is_none());
    }
}
