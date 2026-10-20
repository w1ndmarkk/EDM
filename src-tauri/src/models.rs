use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum DownloadKind {
    #[default]
    File,
    Video,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    #[default]
    Queued,
    Probing,
    Downloading,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

impl DownloadState {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Probing | Self::Downloading)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SegmentInfo {
    pub start: i64,
    /// Inclusive.
    pub end: i64,
    #[serde(default)]
    pub received: i64,
}

impl SegmentInfo {
    pub fn length(&self) -> i64 {
        self.end - self.start + 1
    }
    pub fn is_done(&self) -> bool {
        self.received >= self.length()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadItem {
    pub id: Uuid,
    pub url: String,
    pub file_name: String,
    pub directory: PathBuf,
    pub kind: DownloadKind,
    pub state: DownloadState,
    pub total_bytes: i64,
    pub received_bytes: i64,
    pub supports_ranges: bool,
    pub segments: Vec<SegmentInfo>,
    pub headers: BTreeMap<String, String>,
    pub cookie_file: Option<String>,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub final_path: Option<String>,
    /// 0 = use the setting.
    pub requested_segments: u32,
    /// Video: yt-dlp -f expression chosen on the page (None = settings).
    pub ytdlp_format: Option<String>,
    /// Video: extract audio to this format ("m4a", "mp3").
    pub audio_format: Option<String>,
    /// Video download that produces only audio (goes to the Music category).
    pub audio_only: bool,
    /// Video: file name (without extension) from the page title, for streams yt-dlp cannot name (HLS, DASH).
    pub output_title: Option<String>,
    /// Video: files yt-dlp announced while downloading; their leftovers (.part, -Frag, .ytdl) are deleted on cancel.
    pub temp_paths: Vec<String>,
    /// When the item became paused/failed (ms); partial data is deleted when it is not resumed within 24 hours.
    pub stalled_since: Option<i64>,
    /// Folder inside the category folder (a playlist's name: Video\<playlist>).
    pub subfolder: Option<String>,

    // Not persisted.
    #[serde(skip)]
    pub speed: f64,
    #[serde(skip)]
    pub log: String,
    /// yt-dlp: current phase (downloading video/audio, merging…).
    #[serde(skip)]
    pub stage: String,
    /// The site only lets the browser download this file (bot check): the extension that handed it over takes
    /// it back to the browser.
    #[serde(skip)]
    pub handback: bool,
}

impl Default for DownloadItem {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            url: String::new(),
            file_name: String::new(),
            directory: PathBuf::new(),
            kind: DownloadKind::File,
            state: DownloadState::Queued,
            total_bytes: -1,
            received_bytes: 0,
            supports_ranges: false,
            segments: vec![],
            headers: BTreeMap::new(),
            cookie_file: None,
            error_message: None,
            created_at: crate::util::now_ms(),
            completed_at: None,
            final_path: None,
            requested_segments: 0,
            ytdlp_format: None,
            audio_format: None,
            audio_only: false,
            output_title: None,
            temp_paths: vec![],
            stalled_since: None,
            subfolder: None,
            speed: 0.0,
            log: String::new(),
            stage: String::new(),
            handback: false,
        }
    }
}

impl DownloadItem {
    /// Part file: id based so it stays stable when the name changes.
    pub fn part_path(&self) -> PathBuf {
        self.directory.join(format!(".dm-{}.part", self.id))
    }

    pub fn category(&self) -> FileCategory {
        FileCategory::detect(&self.file_name, self.kind, self.audio_only)
    }

    pub fn host(&self) -> Option<String> {
        reqwest::Url::parse(&self.url).ok().and_then(|u| u.host_str().map(str::to_lowercase))
    }
}

/// What the UI sees (no log, plus derived fields).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ItemView {
    pub id: Uuid,
    pub url: String,
    pub file_name: String,
    pub kind: DownloadKind,
    pub state: DownloadState,
    pub total_bytes: i64,
    pub received_bytes: i64,
    pub supports_ranges: bool,
    pub segments: Vec<SegmentInfo>,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub completed_at: Option<i64>,
    pub final_path: Option<String>,
    pub speed: f64,
    pub stage: String,
    /// Bot-checked file: the extension that handed it over gives it back to the browser.
    pub handback: bool,
    pub category: FileCategory,
    pub destination: String,
}

// MARK: - Category

/// IDM-style categories. Each has a subfolder of the download folder; everything else lands in the folder itself.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[serde(rename_all = "lowercase")]
pub enum FileCategory {
    Compressed,
    Document,
    Music,
    Program,
    Video,
    Other,
}

impl FileCategory {
    /// Subfolder name (English, like IDM, so the folders stay stable whatever the UI language).
    pub fn folder(self) -> Option<&'static str> {
        match self {
            Self::Compressed => Some("Compressed"),
            Self::Document => Some("Documents"),
            Self::Music => Some("Music"),
            Self::Program => Some("Programs"),
            Self::Video => Some("Video"),
            Self::Other => None,
        }
    }

    /// Extension patterns (`*` and `?` allowed).
    fn patterns(self) -> &'static [&'static str] {
        match self {
            Self::Compressed => &[
                "zip", "rar", "r0*", "r1*", "7z", "tar", "gz", "gzip", "tgz", "bz2", "xz", "zst", "lz", "lzh", "lzma", "arj", "ace", "cab", "sit", "sitx", "sea",
                "z", "iso", "img",
            ],
            Self::Document => &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "pps", "ppsx", "odt", "ods", "odp", "rtf", "txt", "csv", "epub", "mobi", "md"],
            Self::Music => &["mp3", "wav", "wma", "mpa", "m4a", "aac", "aif", "aiff", "flac", "ogg", "oga", "opus", "ra", "ram", "mka", "alac"],
            Self::Program => &["exe", "msi", "msu", "msix", "msixbundle", "appx", "appxbundle", "apk", "xapk", "bat", "cmd", "jar", "dmg", "pkg", "deb", "rpm", "appimage"],
            Self::Video => &[
                "avi", "mpg", "mpe", "mpeg", "asf", "wmv", "mov", "qt", "rm", "rmvb", "mp4", "m4v", "flv", "webm", "ogv", "mkv", "ts", "3gp", "3g2", "m2ts", "vob",
            ],
            Self::Other => &[],
        }
    }

    pub fn detect(file_name: &str, kind: DownloadKind, audio_only: bool) -> FileCategory {
        if kind == DownloadKind::Video {
            return if audio_only { Self::Music } else { Self::Video };
        }
        let ext = extension(file_name);
        if ext.is_empty() {
            return Self::Other;
        }
        [Self::Compressed, Self::Document, Self::Music, Self::Program, Self::Video]
            .into_iter()
            .find(|c| c.patterns().iter().any(|p| crate::util::wildcard_match(p, &ext)))
            .unwrap_or(Self::Other)
    }
}

/// Lowercase extension without the dot ("" when there is none).
pub fn extension(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// Extension of the last path segment of a URL.
pub fn url_extension(url: &reqwest::Url) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    extension(&percent_encoding::percent_decode_str(last).decode_utf8_lossy())
}
