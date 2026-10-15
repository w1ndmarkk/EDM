//! yt-dlp wrapper. Prefers a user-installed (updated) yt-dlp, then the bundled copy, then %PATH%.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::i18n::t;
use crate::paths::{bin_dir, bundled_tools_dir, search_path};
use crate::{sitelogin, tf, util};

pub fn locate_ytdlp(custom: &str) -> Option<PathBuf> {
    let mut candidates = vec![];
    let custom = custom.trim().trim_matches('"');
    if !custom.is_empty() {
        candidates.push(PathBuf::from(custom));
    }
    candidates.push(bin_dir().join("yt-dlp.exe"));
    candidates.push(bundled_tools_dir().join("yt-dlp.exe"));
    candidates.into_iter().find(|p| p.is_file()).or_else(|| search_path("yt-dlp.exe"))
}

pub fn locate_ffmpeg() -> Option<PathBuf> {
    Some(bundled_tools_dir().join("ffmpeg.exe")).filter(|p| p.is_file()).or_else(|| search_path("ffmpeg.exe"))
}

/// deno is the JavaScript runtime yt-dlp uses to solve YouTube's n/signature challenges.
/// Without it YouTube downloads fail or lose formats (see https://github.com/yt-dlp/yt-dlp/wiki/EJS).
pub fn locate_deno() -> Option<PathBuf> {
    let mut candidates = vec![bin_dir().join("deno.exe"), bundled_tools_dir().join("deno.exe")];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".deno").join("bin").join("deno.exe"));
    }
    candidates.into_iter().find(|p| p.is_file()).or_else(|| search_path("deno.exe"))
}

pub fn host_matches(host: &str, domains: &[&str]) -> bool {
    let host = host.to_lowercase();
    domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

pub fn is_youtube(host: Option<&str>) -> bool {
    host.is_some_and(|h| host_matches(h, &["youtube.com", "youtu.be", "youtube-nocookie.com"]))
}

/// YouTube rejects the signed-in clients yt-dlp picks when cookies are passed
/// ("The page needs to be reloaded", yt-dlp#17389); the same video usually works without cookies.
pub fn is_signed_in_client_failure(message: &str) -> bool {
    message.to_lowercase().contains("page needs to be reloaded")
}

fn site_login_matches(host: Option<&str>) -> bool {
    let domains = sitelogin::domains();
    let refs: Vec<&str> = domains.iter().map(String::as_str).collect();
    host.is_some_and(|h| host_matches(h, &refs))
}

pub fn has_cookies(item_cookies: Option<&str>, host: Option<&str>) -> bool {
    item_cookies.is_some_and(|c| Path::new(c).is_file()) || site_login_matches(host)
}

/// Combines the per-download cookie file (from the browser extension) with the shared site-login cookie file.
/// The site-login jar is only attached when the target host belongs to a domain the user signed in to.
fn merged_cookie_file(item_cookies: Option<&str>, host: Option<&str>) -> Option<PathBuf> {
    let item = item_cookies.map(PathBuf::from).filter(|p| p.is_file());
    let site = site_login_matches(host).then(sitelogin::cookie_file_path).flatten();
    match (item, site) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(a), Some(b)) => {
            let body = format!("{}\n{}", std::fs::read_to_string(&b).unwrap_or_default(), std::fs::read_to_string(&a).unwrap_or_default());
            let tmp = std::env::temp_dir().join(format!("edm-cookies-{}.txt", uuid::Uuid::new_v4()));
            if std::fs::write(&tmp, body).is_ok() { Some(tmp) } else { Some(a) }
        }
    }
}

pub struct Progress {
    pub downloaded: i64,
    pub total: i64,
    pub speed: f64,
}

pub enum VideoEvent {
    Progress(Progress),
    Line(String),
}

/// Turns a yt-dlp log line into a user-facing phase description, if it marks a new phase.
pub fn stage_for(line: &str) -> Option<String> {
    static AUDIO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.f[0-9]+[-a-z0-9]*\.(m4a|webm|opus|mp3|aac|ogg)$").unwrap());
    static PART: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.f[0-9]+[-a-z0-9]*\.[a-z0-9]+$").unwrap());
    if let Some(name) = line.strip_prefix("[download] Destination: ") {
        // yt-dlp names intermediate files <title>.f<formatid>.<ext>
        let lower = name.to_lowercase();
        if AUDIO.is_match(&lower) {
            return Some(t("Downloading audio…"));
        }
        if PART.is_match(&lower) {
            return Some(t("Downloading video…"));
        }
        return Some(t("Downloading…"));
    }
    if line.starts_with("[Merger]") {
        return Some(t("Merging video and audio (ffmpeg)…"));
    }
    if line.starts_with("[VideoRemuxer]") || line.starts_with("[VideoConvertor]") {
        return Some(t("Converting container (ffmpeg)…"));
    }
    if line.starts_with("[ExtractAudio]") {
        return Some(t("Extracting audio (ffmpeg)…"));
    }
    if line.starts_with("MCFMT|") {
        let p: Vec<&str> = line.split('|').collect();
        if p.len() < 6 {
            return None;
        }
        let mut parts: Vec<String> = vec![];
        if p[1] != "NA" && !p[1].is_empty() {
            parts.push(p[1].to_string());
        }
        if p[2] != "NA" {
            parts.push(p[2].to_uppercase());
        }
        if p[3] != "NA" && p[3] != "none" {
            if let Some(v) = p[3].split('.').next() {
                parts.push(v.to_string());
            }
        }
        if let Ok(n) = p[5].parse::<f64>() {
            if n > 0.0 {
                parts.push(format!("~{}", util::fmt_size(n as i64)));
            }
        }
        return Some(tf!("Selected: {0}", parts.join(" · ")));
    }
    if line.contains("has already been downloaded") {
        return Some(t("File already downloaded"));
    }
    None
}

/// Size estimate from the MCFMT line (`filesize_approx`).
pub fn approx_size(line: &str) -> Option<i64> {
    let p: Vec<&str> = line.strip_prefix("MCFMT|")?.split('|').collect();
    p.get(4).and_then(|s| s.parse::<f64>().ok()).filter(|n| *n > 0.0).map(|n| n as i64)
}

/// ffmpeg, User-Agent/Referer, cookies and the JavaScript runtime: everything yt-dlp needs to reach the page.
fn source_arguments(url: &str, headers: &BTreeMap<String, String>, cookie_file: Option<&str>, use_cookies: bool) -> Vec<String> {
    let mut args: Vec<String> = vec![];
    if let Some(ff) = locate_ffmpeg() {
        args.extend(["--ffmpeg-location".into(), ff.to_string_lossy().into_owned()]);
    }
    if let Some(ua) = headers.get("User-Agent").filter(|s| !s.is_empty()) {
        args.extend(["--user-agent".into(), ua.clone()]);
    }
    if let Some(r) = headers.get("Referer").filter(|s| !s.is_empty()) {
        args.extend(["--referer".into(), r.clone()]);
    }
    let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string));
    if use_cookies {
        if let Some(cf) = merged_cookie_file(cookie_file, host.as_deref()) {
            args.extend(["--cookies".into(), cf.to_string_lossy().into_owned()]);
        }
    }
    if let Some(deno) = locate_deno() {
        args.extend(["--js-runtimes".into(), format!("deno:{}", deno.to_string_lossy())]);
    }
    args
}

/// A hidden-console yt-dlp process with UTF-8 output and the bundled tools on %PATH%.
fn command(executable: &Path, args: &[String]) -> tokio::process::Command {
    let mut path_env = std::ffi::OsString::from(bundled_tools_dir());
    if let Some(p) = std::env::var_os("PATH") {
        path_env.push(";");
        path_env.push(p);
    }
    let mut cmd = tokio::process::Command::new(executable);
    cmd.args(args)
        .env("PATH", path_env)
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(util::CREATE_NO_WINDOW);
    cmd
}

// MARK: - Formats for the page's download panel

/// One choice in the "Download this video" panel.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FormatOption {
    pub label: String,
    pub detail: String,
    /// yt-dlp -f expression (video choices).
    pub format: Option<String>,
    /// Audio-only extraction format (audio choices).
    pub audio: Option<String>,
    pub size: i64,
    pub is_audio: bool,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FormatList {
    /// yt-dlp extractor ("Youtube", "Vimeo", … or "Generic" when it only found a plain media link).
    pub extractor: String,
    pub title: String,
    pub duration: f64,
    pub options: Vec<FormatOption>,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistEntry {
    /// 1-based position in the playlist.
    pub index: usize,
    pub url: String,
    pub title: String,
    pub duration: f64,
    pub uploader: String,
    /// Private or deleted videos are listed by YouTube but cannot be downloaded.
    pub available: bool,
}

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub title: String,
    pub uploader: String,
    pub entries: Vec<PlaylistEntry>,
}

/// Reads a playlist's entries without visiting each video (`--flat-playlist -J`).
pub async fn list_playlist(executable: &Path, url: &str, use_cookies: bool) -> Result<Playlist, String> {
    let mut args: Vec<String> =
        ["-J", "--flat-playlist", "--yes-playlist", "--no-warnings", "--no-colors"].iter().map(|s| s.to_string()).collect();
    args.extend(source_arguments(url, &BTreeMap::new(), None, use_cookies));
    args.push(url.to_string());
    let mut cmd = command(executable, &args);
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(180), cmd.output())
        .await
        .map_err(|_| t("yt-dlp did not answer in time."))?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| l.starts_with("ERROR")).or_else(|| err.lines().rev().find(|l| !l.trim().is_empty()));
        return Err(line.unwrap_or("yt-dlp failed").trim().to_string());
    }
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    parse_playlist(&info).ok_or_else(|| t("This address is not a playlist."))
}

fn parse_playlist(info: &serde_json::Value) -> Option<Playlist> {
    if info["_type"] != "playlist" {
        return None;
    }
    let text = |v: &serde_json::Value, keys: &[&str]| keys.iter().find_map(|k| v[*k].as_str().filter(|s| !s.is_empty())).unwrap_or("").to_string();
    let entries: Vec<PlaylistEntry> = info["entries"]
        .as_array()?
        .iter()
        .filter(|e| e.is_object())
        .enumerate()
        .filter_map(|(i, e)| {
            let url = text(e, &["url", "webpage_url"]);
            let url = if url.starts_with("http") {
                url
            } else if e["ie_key"] == "Youtube" && !text(e, &["id"]).is_empty() {
                format!("https://www.youtube.com/watch?v={}", text(e, &["id"]))
            } else {
                return None;
            };
            let title = text(e, &["title"]);
            // "[Private video]", "[Deleted video]"
            let hidden = title.starts_with('[') && title.ends_with(']') && e["duration"].is_null();
            Some(PlaylistEntry {
                index: e["playlist_index"].as_u64().map(|n| n as usize).unwrap_or(i + 1),
                title: if title.is_empty() { text(e, &["id"]) } else { title },
                url,
                duration: e["duration"].as_f64().unwrap_or(0.0),
                uploader: text(e, &["channel", "uploader"]),
                available: !hidden && e["availability"].as_str().map_or(true, |a| !matches!(a, "private" | "needs_auth" | "premium_only" | "subscriber_only")),
            })
        })
        .collect();
    Some(Playlist { title: text(info, &["title", "id"]), uploader: text(info, &["channel", "uploader"]), entries })
}

/// yt-dlp -f expression for a quality picked in the playlist dialog ("best", "1080", …); None for audio or the settings.
pub fn format_for_quality(quality: &str, container: &str) -> Option<String> {
    let h = match quality {
        // Highest resolution whatever the codec (YouTube has H.264 only up to 1080p).
        "best" => return Some("bv*+ba/b".into()),
        q if q.parse::<u32>().is_ok() => format!("[height<={q}]"),
        _ => return None,
    };
    // Up to 1080p, H.264 + AAC first so the file plays everywhere.
    Some(if container == "mp4" {
        format!("bv*{h}[vcodec^=avc1]+ba[acodec^=mp4a]/bv*{h}[ext=mp4]+ba[ext=m4a]/bv*{h}+ba/b{h}/b")
    } else {
        format!("bv*{h}+ba/b{h}/b")
    })
}

/// Asks yt-dlp for the page's formats (`-J`) and turns them into one choice per resolution plus audio-only choices.
pub async fn list_formats(
    executable: &Path,
    url: &str,
    headers: &BTreeMap<String, String>,
    cookie_file: Option<&str>,
    use_cookies: bool,
    container: &str,
) -> Result<FormatList, String> {
    let mut args: Vec<String> = ["-J", "--no-playlist", "--no-warnings", "--no-colors"].iter().map(|s| s.to_string()).collect();
    args.extend(source_arguments(url, headers, cookie_file, use_cookies));
    args.push(url.to_string());
    let mut cmd = command(executable, &args);
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(90), cmd.output())
        .await
        .map_err(|_| t("yt-dlp did not answer in time."))?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let line = err.lines().rev().find(|l| l.starts_with("ERROR")).or_else(|| err.lines().rev().find(|l| !l.trim().is_empty()));
        return Err(line.unwrap_or("yt-dlp failed").trim().to_string());
    }
    let mut info: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    if info["_type"] == "playlist" {
        info = info["entries"].get(0).cloned().unwrap_or_default();
    }
    Ok(build_format_list(&info, container))
}

fn build_format_list(info: &serde_json::Value, container: &str) -> FormatList {
    use serde_json::Value;
    let duration = info["duration"].as_f64().unwrap_or(0.0);
    let formats: Vec<&Value> = info["formats"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    let codec = |f: &Value, key: &str| f[key].as_str().unwrap_or("").to_lowercase();
    let size = |f: &Value| -> i64 {
        f["filesize"]
            .as_f64()
            .or_else(|| f["filesize_approx"].as_f64())
            .or_else(|| f["tbr"].as_f64().map(|tbr| tbr * duration * 125.0))
            .unwrap_or(0.0) as i64
    };
    let height = |f: &Value| f["height"].as_i64().unwrap_or(0);
    let is_video = |f: &Value| height(f) > 0 && codec(f, "vcodec") != "none" && f["ext"] != "mhtml";
    let is_audio_only = |f: &Value| codec(f, "vcodec") == "none" && !codec(f, "acodec").is_empty() && codec(f, "acodec") != "none";
    let mp4 = container == "mp4";

    // Best audio track (AAC first when the result is an MP4).
    let rate = |f: &Value, key: &str| f[key].as_f64().unwrap_or(0.0);
    let audio: Vec<&Value> = formats.iter().copied().filter(|f| is_audio_only(f)).collect();
    let best_audio: Option<&Value> = audio
        .iter()
        .copied()
        .filter(|f| !mp4 || codec(f, "acodec").starts_with("mp4a"))
        .max_by(|a, b| rate(a, "abr").total_cmp(&rate(b, "abr")))
        .or_else(|| audio.iter().copied().max_by(|a, b| rate(a, "abr").total_cmp(&rate(b, "abr"))));

    let mut heights: Vec<i64> = formats.iter().filter(|f| is_video(f)).map(|f| height(f)).collect();
    heights.sort_unstable_by(|a, b| b.cmp(a));
    heights.dedup();

    let mut options = vec![];
    for h in heights.into_iter().take(8) {
        let at: Vec<&Value> = formats.iter().copied().filter(|f| is_video(f) && height(f) == h).collect();
        let preferred = at
            .iter()
            .copied()
            .filter(|f| !mp4 || codec(f, "vcodec").starts_with("avc1"))
            .max_by(|a, b| rate(a, "tbr").total_cmp(&rate(b, "tbr")))
            .or_else(|| at.iter().copied().max_by(|a, b| rate(a, "tbr").total_cmp(&rate(b, "tbr"))));
        let Some(v) = preferred else { continue };
        let needs_audio = codec(v, "acodec") == "none";
        let total = size(v) + if needs_audio { best_audio.map(|a| size(a)).unwrap_or(0) } else { 0 };
        let fps = v["fps"].as_f64().unwrap_or(0.0);
        let label = if fps > 30.5 { format!("{h}p{}", fps.round() as i64) } else { format!("{h}p") };
        let ext = if container == "original" { v["ext"].as_str().unwrap_or("").to_uppercase() } else { container.to_uppercase() };
        let format = if mp4 {
            format!("bv*[height={h}][vcodec^=avc1]+ba[acodec^=mp4a]/bv*[height={h}]+ba/b[height={h}]/bv*[height<={h}]+ba/b")
        } else {
            format!("bv*[height={h}]+ba/b[height={h}]/bv*[height<={h}]+ba/b")
        };
        let detail = if total > 0 { format!("{ext} · ~{}", util::fmt_size(total)) } else { ext };
        options.push(FormatOption { label, detail, format: Some(format), audio: None, size: total, is_audio: false });
    }

    let has_audio = best_audio.is_some() || formats.iter().any(|f| !codec(f, "acodec").is_empty() && codec(f, "acodec") != "none");
    if has_audio || options.is_empty() {
        let audio_size = best_audio.map(|a| size(a)).unwrap_or(0);
        for fmt in ["m4a", "mp3"] {
            let detail = if audio_size > 0 { format!("{} · ~{}", fmt.to_uppercase(), util::fmt_size(audio_size)) } else { fmt.to_uppercase() };
            options.push(FormatOption { label: t("Audio only"), detail, format: None, audio: Some(fmt.into()), size: audio_size, is_audio: true });
        }
    }
    FormatList {
        extractor: info["extractor_key"].as_str().unwrap_or("").to_string(),
        title: info["title"].as_str().unwrap_or("").to_string(),
        duration,
        options,
    }
}

pub struct RunOptions<'a> {
    pub executable: &'a Path,
    pub url: &'a str,
    pub output_dir: &'a Path,
    /// File name without extension; None = yt-dlp's "<title> [<id>]".
    pub output_name: Option<&'a str>,
    pub format_args: Vec<String>,
    pub headers: &'a BTreeMap<String, String>,
    pub cookie_file: Option<&'a str>,
    pub use_cookies: bool,
}

/// Starts yt-dlp and returns its process id. `on_finish` receives the final file path (if yt-dlp printed it).
pub fn run(
    opts: RunOptions,
    on_event: impl Fn(VideoEvent) + Send + Sync + 'static,
    on_finish: impl FnOnce(Result<Option<String>, String>) + Send + 'static,
) -> Result<u32, String> {
    // A stream URL (HLS/DASH) has no title of its own: name the file after the page instead.
    let template = match opts.output_name {
        Some(name) => opts.output_dir.join(format!("{}.%(ext)s", name.replace('%', "%%"))),
        None => opts.output_dir.join("%(title).150B [%(id)s].%(ext)s"),
    };
    let mut args: Vec<String> = [
        "--newline", "--no-playlist", "--no-colors", "--progress", "--no-simulate", "--no-mtime", "--no-quiet", "-N", "4",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend([
        "-o".into(),
        template.to_string_lossy().into_owned(),
        "--progress-template".into(),
        "download:DMP|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s".into(),
        "--print".into(),
        "after_move:filepath".into(),
        "--print".into(),
        "before_dl:MCFMT|%(resolution)s|%(ext)s|%(vcodec)s|%(acodec)s|%(filesize_approx)s|%(format_id)s|%(duration)s".into(),
    ]);
    args.extend(opts.format_args);
    args.extend(source_arguments(opts.url, opts.headers, opts.cookie_file, opts.use_cookies));
    args.push(opts.url.to_string());

    let mut cmd = command(opts.executable, &args);
    cmd.current_dir(opts.output_dir);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let pid = child.id().unwrap_or(0);

    let state: Arc<Mutex<(Option<String>, Option<String>)>> = Arc::new(Mutex::new((None, None)));
    let on_event: Arc<dyn Fn(VideoEvent) + Send + Sync> = Arc::new(on_event);
    let out = child.stdout.take().map(|s| tokio::spawn(read_lines(s, false, state.clone(), on_event.clone())));
    let err = child.stderr.take().map(|s| tokio::spawn(read_lines(s, true, state.clone(), on_event.clone())));

    tokio::spawn(async move {
        let status = child.wait().await;
        // Wait for the pipes to drain, but not forever: a grandchild (ffmpeg) may keep them open.
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            if let Some(h) = out {
                let _ = h.await;
            }
            if let Some(h) = err {
                let _ = h.await;
            }
        })
        .await;
        let (path, last_error) = state.lock().unwrap().clone();
        match status {
            Ok(s) if s.success() => on_finish(Ok(path)),
            Ok(s) => on_finish(Err(last_error.unwrap_or_else(|| tf!("yt-dlp exited with code {0}", s.code().unwrap_or(-1))))),
            Err(e) => on_finish(Err(e.to_string())),
        }
    });
    Ok(pid)
}

async fn read_lines(
    stream: impl AsyncRead + Unpin,
    is_error: bool,
    state: Arc<Mutex<(Option<String>, Option<String>)>>,
    on_event: Arc<dyn Fn(VideoEvent) + Send + Sync>,
) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&buf);
        for raw in text.split('\r') {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("DMP|") {
                let p: Vec<&str> = rest.split('|').collect();
                if p.len() >= 4 {
                    let num = |s: &str| s.parse::<f64>().map(|v| v as i64).unwrap_or(-1);
                    let mut total = num(p[1]);
                    if total <= 0 {
                        total = num(p[2]);
                    }
                    on_event(VideoEvent::Progress(Progress { downloaded: num(p[0]).max(0), total, speed: p[3].parse::<f64>().unwrap_or(0.0).max(0.0) }));
                }
                continue;
            }
            if Path::new(line).is_absolute() && Path::new(line).is_file() {
                state.lock().unwrap().0 = Some(line.to_string());
            }
            if is_error && line.starts_with("ERROR") {
                state.lock().unwrap().1 = Some(line.to_string());
            }
            on_event(VideoEvent::Line(line.to_string()));
        }
    }
}

/// `--version` output of a tool (first line), run without a console window.
pub fn tool_version(path: &Path) -> Option<String> {
    let out = util::command(path).arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    // deno prints three lines (deno, v8, typescript); keep the first.
    text.lines().next().map(|l| l.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_flat_playlists() {
        let info = serde_json::json!({
            "_type": "playlist", "title": "Lessons", "channel": "Teacher",
            "entries": [
                {"ie_key": "Youtube", "id": "aaa", "url": "https://www.youtube.com/watch?v=aaa", "title": "One", "duration": 61.0},
                {"ie_key": "Youtube", "id": "bbb", "url": "bbb", "title": "[Private video]", "duration": null},
                {"ie_key": "Youtube", "id": "ccc", "url": "https://www.youtube.com/watch?v=ccc", "title": "Three", "duration": 5.0, "playlist_index": 3}
            ]
        });
        let p = parse_playlist(&info).unwrap();
        assert_eq!((p.title.as_str(), p.uploader.as_str(), p.entries.len()), ("Lessons", "Teacher", 3));
        assert_eq!(p.entries[1].url, "https://www.youtube.com/watch?v=bbb");
        assert!(p.entries[0].available && !p.entries[1].available);
        assert_eq!(p.entries.iter().map(|e| e.index).collect::<Vec<_>>(), [1, 2, 3]);
        assert!(parse_playlist(&serde_json::json!({"_type": "video", "title": "x"})).is_none());
    }

    #[test]
    fn playlist_qualities() {
        assert_eq!(format_for_quality("best", "mp4").unwrap(), "bv*+ba/b");
        assert!(format_for_quality("720", "mp4").unwrap().starts_with("bv*[height<=720][vcodec^=avc1]"));
        assert_eq!(format_for_quality("480", "mkv").unwrap(), "bv*[height<=480]+ba/b[height<=480]/b");
        assert!(format_for_quality("settings", "mp4").is_none());
    }

    #[test]
    fn stages() {
        assert!(stage_for("[Merger] Merging formats into \"x.mp4\"").is_some());
        assert!(stage_for("[youtube] abc: Downloading webpage").is_none());
        assert_eq!(approx_size("MCFMT|1920x1080|mp4|avc1.640028|mp4a.40.2|123456|137+140|60"), Some(123456));
        assert!(is_youtube(Some("www.youtube.com")));
        assert!(approx_size("MCFMT|x").is_none());
    }

    #[test]
    fn format_list() {
        let info = serde_json::json!({
            "title": "Clip", "duration": 10.0,
            "formats": [
                {"format_id": "sb0", "ext": "mhtml", "height": 90, "vcodec": "none", "acodec": "none"},
                {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "abr": 128.0, "filesize": 160000},
                {"format_id": "251", "ext": "webm", "vcodec": "none", "acodec": "opus", "abr": 160.0, "filesize": 200000},
                {"format_id": "137", "ext": "mp4", "height": 1080, "fps": 30, "vcodec": "avc1.640028", "acodec": "none", "tbr": 4000.0, "filesize": 5000000},
                {"format_id": "248", "ext": "webm", "height": 1080, "fps": 30, "vcodec": "vp9", "acodec": "none", "tbr": 3000.0},
                {"format_id": "18", "ext": "mp4", "height": 360, "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "tbr": 500.0, "filesize": 600000}
            ]
        });
        let list = build_format_list(&info, "mp4");
        assert_eq!(list.title, "Clip");
        let labels: Vec<&str> = list.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(&labels[..2], &["1080p", "360p"]);
        // 1080p avc1 video + AAC audio; 360p is progressive (already has audio).
        assert_eq!(list.options[0].size, 5_160_000);
        assert_eq!(list.options[1].size, 600_000);
        assert_eq!(list.options.iter().filter(|o| o.is_audio).count(), 2);
        assert!(!is_youtube(Some("notyoutube.com")));
    }
}
