use std::path::PathBuf;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use crate::models::{extension, url_extension};
use crate::util::wildcard_match;

/// IDM's default "Automatically start downloading the following file types" list.
pub const DEFAULT_CAPTURE_TYPES: &str = "3GP 7Z AAC ACE AIF APK ARJ ASF AVI BIN BZ2 EXE GZ GZIP IMG ISO LZH M4A M4V MKV MOV MP3 MP4 MPA MPE MPEG MPG MSI MSU OGG OGV PDF PLJ PPS PPT QT R0* R1* RA RAR RM RMVB SEA SIT SITX TAR TIF TIFF WAV WMA WMV Z ZIP";
/// IDM's default "Don't start downloading automatically from the following sites" list.
pub const DEFAULT_EXCLUDED_SITES: &str =
    "*.update.microsoft.com download.windowsupdate.com *.download.windowsupdate.com siteseal.thawte.com ecom.cimetz.com *.voice2page.com localhost";

fn tokens(list: &str) -> impl Iterator<Item = &str> {
    list.split(|c: char| c.is_whitespace() || c == ',' || c == ';').map(|t| t.trim_matches('.')).filter(|t| !t.is_empty())
}

/// Uppercase/lowercase, de-duplicated, space separated.
fn normalize_list(list: &str, upper: bool) -> String {
    let mut out: Vec<String> = vec![];
    for t in tokens(list) {
        let t = if upper { t.to_uppercase() } else { t.to_lowercase() };
        if !out.contains(&t) {
            out.push(t);
        }
    }
    out.join(" ")
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub segment_count: u32,
    pub max_concurrent: u32,
    pub server_port: u16,
    pub download_directory: String,
    pub ytdlp_path: String,
    /// Custom -f expression (overrides the pickers when non-empty).
    pub video_format: String,
    /// mp4 | mkv | webm | original
    pub video_container: String,
    /// 0 = best, -1 = audio m4a, -2 = audio mp3
    pub video_max_height: i32,
    pub auto_start: bool,
    pub notify_on_complete: bool,
    pub show_detail_on_start: bool,
    pub auto_close_detail: bool,
    pub watch_clipboard: bool,
    /// Extensions handed to the app, space separated, `*`/`?` wildcards ("ZIP R0* MP4").
    pub capture_types: String,
    /// Hosts never captured, space separated, wildcards ("*.update.microsoft.com").
    pub excluded_sites: String,
    /// Addresses never captured (URL prefixes or wildcard patterns), one per entry.
    pub excluded_addresses: Vec<String>,
    pub onboarding_done: bool,
    /// Compressed / Documents / Music / Programs / Video subfolders; other files go to the folder itself.
    pub organize_by_category: bool,
    pub check_for_updates: bool,
    /// Keep the main window closed when the app is started by hand (a Windows-startup launch is always hidden).
    pub start_in_tray: bool,
    /// One-shot flag: "start with Windows" was turned on automatically on the first run of the installed app.
    pub autostart_auto_registered: bool,
    /// The user chose "Don't ask again" on the deno install prompt for YouTube downloads.
    pub deno_prompt_suppressed: bool,
    pub last_update_check: i64,
    pub skipped_version: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            segment_count: 8,
            max_concurrent: 3,
            server_port: 48213,
            download_directory: crate::paths::default_download_dir().to_string_lossy().into_owned(),
            ytdlp_path: String::new(),
            video_format: String::new(),
            video_container: "mp4".into(),
            video_max_height: 1080,
            auto_start: true,
            notify_on_complete: true,
            show_detail_on_start: true,
            auto_close_detail: false,
            watch_clipboard: true,
            capture_types: DEFAULT_CAPTURE_TYPES.into(),
            excluded_sites: DEFAULT_EXCLUDED_SITES.into(),
            excluded_addresses: vec![],
            onboarding_done: false,
            organize_by_category: true,
            check_for_updates: true,
            start_in_tray: false,
            autostart_auto_registered: false,
            deno_prompt_suppressed: false,
            last_update_check: 0,
            skipped_version: String::new(),
        }
    }
}

impl Settings {
    fn sanitized(mut self) -> Self {
        self.segment_count = self.segment_count.clamp(1, 16);
        self.max_concurrent = self.max_concurrent.clamp(1, 10);
        self.server_port = self.server_port.max(1024);
        if self.download_directory.trim().is_empty() {
            self.download_directory = crate::paths::default_download_dir().to_string_lossy().into_owned();
        }
        if !["mp4", "mkv", "webm", "original"].contains(&self.video_container.as_str()) {
            self.video_container = "mp4".into();
        }
        self.capture_types = normalize_list(&self.capture_types, true);
        self.excluded_sites = normalize_list(&self.excluded_sites, false);
        self.excluded_addresses = self.excluded_addresses.iter().map(|a| a.trim().to_string()).filter(|a| !a.is_empty()).collect();
        self.excluded_addresses.dedup();
        self
    }

    /// Whether a browser download or a copied link should be handed to the app (IDM rules: excluded sites,
    /// excluded addresses, then the file type list). `file_name` is the browser's suggested name, if any.
    pub fn should_capture(&self, url: &reqwest::Url, file_name: Option<&str>) -> bool {
        let host = url.host_str().unwrap_or("").to_lowercase();
        if tokens(&self.excluded_sites).any(|p| wildcard_match(p, &host)) {
            return false;
        }
        let full = url.as_str().to_lowercase();
        let excluded = self.excluded_addresses.iter().any(|p| {
            if p.contains(['*', '?']) { wildcard_match(p, &full) } else { full.starts_with(&p.to_lowercase()) }
        });
        if excluded {
            return false;
        }
        let ext = file_name.map(extension).filter(|e| !e.is_empty()).unwrap_or_else(|| url_extension(url));
        !ext.is_empty() && tokens(&self.capture_types).any(|p| wildcard_match(p, &ext))
    }

    /// The capture rules for the browser extension, which applies them itself (Extension/rules.js mirrors
    /// `should_capture`) so it can decide before the browser downloads anything.
    pub fn capture_rules(&self) -> serde_json::Value {
        let list = |s: &str| tokens(s).map(str::to_lowercase).collect::<Vec<_>>();
        serde_json::json!({
            "captureTypes": list(&self.capture_types),
            "excludedSites": list(&self.excluded_sites),
            "excludedAddresses": self.excluded_addresses.iter().map(|a| a.trim().to_lowercase()).filter(|a| !a.is_empty()).collect::<Vec<_>>(),
        })
    }

    pub fn download_dir(&self) -> PathBuf {
        PathBuf::from(&self.download_directory)
    }

    /// The default video download produces only audio (Audio only M4A/MP3 picked in Settings ▸ Video).
    pub fn default_video_is_audio(&self) -> bool {
        self.video_format.trim().is_empty() && self.video_max_height < 0
    }

    /// `--merge-output-format`/`--remux-video` for the chosen container.
    fn merge_arguments(&self) -> Vec<String> {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        match self.video_container.as_str() {
            "mp4" => s(&["--merge-output-format", "mp4", "--remux-video", "mp4"]),
            "webm" => s(&["--merge-output-format", "webm"]),
            "mkv" => s(&["--merge-output-format", "mkv"]),
            _ => vec![],
        }
    }

    fn audio_arguments(format: &str) -> Vec<String> {
        ["-f", "ba/b", "-x", "--audio-format", format, "--audio-quality", "0"].iter().map(|x| x.to_string()).collect()
    }

    /// yt-dlp arguments for a download: a format picked on the page wins over the settings.
    pub fn ytdlp_arguments_for(&self, format: Option<&str>, audio: Option<&str>) -> Vec<String> {
        if let Some(a) = audio {
            return Self::audio_arguments(a);
        }
        if let Some(f) = format {
            let mut args = vec!["-f".to_string(), f.to_string()];
            args.extend(self.merge_arguments());
            return args;
        }
        self.ytdlp_format_arguments()
    }

    /// yt-dlp arguments for the chosen format/quality.
    pub fn ytdlp_format_arguments(&self) -> Vec<String> {
        let custom = self.video_format.trim();
        if !custom.is_empty() {
            let merge = if self.video_container == "original" { "mkv" } else { self.video_container.as_str() };
            return vec!["-f".into(), custom.into(), "--merge-output-format".into(), merge.into()];
        }
        match self.video_max_height {
            -1 => return Self::audio_arguments("m4a"),
            -2 => return Self::audio_arguments("mp3"),
            _ => {}
        }
        let h = if self.video_max_height > 0 { format!("[height<={}]", self.video_max_height) } else { String::new() };
        let expr = match self.video_container.as_str() {
            // Prefer H.264 + AAC so the file plays everywhere; fall back to anything and merge into mp4.
            "mp4" => format!("bv*{h}[vcodec^=avc1]+ba[acodec^=mp4a]/bv*{h}[ext=mp4]+ba[ext=m4a]/bv*{h}+ba/b{h}/b"),
            "webm" => format!("bv*{h}[ext=webm]+ba[ext=webm]/bv*{h}+ba/b{h}/b"),
            _ => format!("bv*{h}+ba/b{h}/b"),
        };
        let mut args = vec!["-f".to_string(), expr];
        args.extend(self.merge_arguments());
        args
    }
}

pub struct SettingsStore {
    inner: RwLock<Settings>,
    path: PathBuf,
}

impl SettingsStore {
    pub fn load() -> Self {
        let path = crate::paths::data_dir().join("settings.json");
        let settings = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .unwrap_or_default()
            .sanitized();
        Self { inner: RwLock::new(settings), path }
    }

    pub fn get(&self) -> Settings {
        self.inner.read().unwrap().clone()
    }

    /// Mutates, saves and broadcasts the settings. Returns the new value.
    pub fn update(&self, app: &AppHandle, f: impl FnOnce(&mut Settings)) -> Settings {
        let new = {
            let mut s = self.inner.write().unwrap();
            let mut copy = s.clone();
            f(&mut copy);
            *s = copy.sanitized();
            s.clone()
        };
        self.save(&new);
        let _ = app.emit("settings", &new);
        new
    }

    /// Applies a partial JSON object from the UI (`{"segmentCount": 6}`).
    pub fn patch(&self, app: &AppHandle, patch: serde_json::Value) -> Result<Settings, String> {
        let mut current = serde_json::to_value(self.get()).map_err(|e| e.to_string())?;
        if let (Some(obj), Some(p)) = (current.as_object_mut(), patch.as_object()) {
            for (k, v) in p {
                obj.insert(k.clone(), v.clone());
            }
        }
        let merged: Settings = serde_json::from_value(current).map_err(|e| e.to_string())?;
        Ok(self.update(app, |s| *s = merged))
    }

    fn save(&self, s: &Settings) {
        if let Ok(data) = serde_json::to_vec_pretty(s) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, data).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same cases the extension's rules.js is checked against: both must decide alike.
    #[test]
    fn rules_shared_with_the_extension() {
        let s = Settings {
            capture_types: "3GP 7Z AAC EXE ISO MP4 MSI PDF R0* R1* RAR ZIP".into(),
            excluded_sites: "*.update.microsoft.com localhost".into(),
            excluded_addresses: vec!["https://example.com/skip/".into(), "https://*.cdn.test/*.zip".into()],
            ..Default::default()
        };
        let cases = [
            ("https://a.com/x.zip", None, true),
            ("https://a.com/x.ZIP?y=1", None, true),
            ("https://a.com/dl?id=3", Some("Setup.exe"), true),
            ("https://a.com/x.r05", None, true),
            ("https://a.com/x.r15", None, true),
            ("https://a.com/x.r20", None, false),
            ("https://a.com/page.html", None, false),
            ("https://a.com/dir/", None, false),
            ("https://a.com/.zip", None, false),
            ("https://x.update.microsoft.com/k.msi", None, false),
            ("http://localhost/a.zip", None, false),
            ("https://example.com/skip/a.zip", None, false),
            ("https://example.com/keep/a.zip", None, true),
            ("https://x.cdn.test/f/a.zip", None, false),
            ("https://a.com/%C3%A7ay%20list.pdf", None, true),
            ("https://a.com/a.zip", Some("notes.txt"), false),
        ];
        for (u, name, want) in cases {
            assert_eq!(s.should_capture(&reqwest::Url::parse(u).unwrap(), name), want, "{u} {name:?}");
        }
        let r = s.capture_rules();
        assert_eq!(r["captureTypes"][0], "3gp");
        assert_eq!(r["excludedAddresses"][1], "https://*.cdn.test/*.zip");
    }

    fn url(s: &str) -> reqwest::Url {
        reqwest::Url::parse(s).unwrap()
    }

    #[test]
    fn capture_rules_follow_idm() {
        let mut s = Settings::default();
        s.excluded_addresses = vec!["https://example.com/private/".into(), "*/nocapture/*".into()];
        let s = s.sanitized();
        assert!(s.should_capture(&url("https://example.com/a/file.ZIP"), None));
        assert!(s.should_capture(&url("https://example.com/a/part.r01"), None));
        assert!(s.should_capture(&url("https://example.com/download?id=3"), Some("setup.exe")));
        assert!(!s.should_capture(&url("https://example.com/page.html"), None));
        assert!(!s.should_capture(&url("https://x.update.microsoft.com/kb.msu"), None));
        assert!(!s.should_capture(&url("http://localhost/a.zip"), None));
        assert!(!s.should_capture(&url("https://example.com/private/a.zip"), None));
        assert!(!s.should_capture(&url("https://cdn.example.org/nocapture/a.zip"), None));
    }

    #[test]
    fn lists_are_normalized() {
        let mut s = Settings::default();
        s.capture_types = "zip, .rar  zip mp4".into();
        assert_eq!(s.sanitized().capture_types, "ZIP RAR MP4");
    }
}
