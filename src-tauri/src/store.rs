//! All downloads: queue, persistence, speed measurement and the yt-dlp / segmented download tasks.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::i18n::t;
use crate::models::{extension, DownloadItem, DownloadKind, DownloadState, ItemView};
use crate::prompt::{self, PromptSpec};
use crate::segmented::{self, DlError, Event, ProbeResult};
use crate::settings::{Settings, SettingsStore};
use crate::video::{self, RunOptions, VideoEvent};
use crate::{tf, tools, tray, util, windows};

pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// Cookie as sent by the browser extension (`chrome.cookies` shape).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub secure: Option<bool>,
    #[serde(default)]
    pub expiration_date: Option<f64>,
}

#[derive(Default, Clone)]
pub struct NewDownload {
    pub url: String,
    pub file_name: Option<String>,
    pub referer: Option<String>,
    pub user_agent: Option<String>,
    pub cookie_header: Option<String>,
    pub cookies: Vec<ExtensionCookie>,
    pub kind: DownloadKind,
    pub extra_headers: BTreeMap<String, String>,
    pub segments: Option<u32>,
    /// Video: yt-dlp -f expression picked in the page's download panel.
    pub format: Option<String>,
    /// Video: audio-only download in this format ("m4a", "mp3").
    pub audio_format: Option<String>,
    /// Video: name for the file (page title) when yt-dlp has none to offer (HLS/DASH stream URLs).
    pub title: Option<String>,
    /// Video: playlist text (HLS/DASH) for streams the page handed to its player as a blob: URL.
    pub manifest: Option<String>,
    /// Folder inside the category folder (playlist downloads: Video\<playlist>).
    pub subfolder: Option<String>,
    /// Batch adds (playlists): no download window per item.
    pub quiet: bool,
}

/// Partial data of a paused or failed download is deleted when it is not resumed within this time.
const STALE_PARTIAL_MS: i64 = 24 * 3600 * 1000;

#[derive(Serialize, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub active_count: usize,
    pub unfinished_count: usize,
    /// Byte-weighted progress of active downloads with a known size.
    pub overall_progress: Option<f64>,
    pub total_speed: f64,
}

struct Task {
    generation: u64,
    cancel: Option<CancellationToken>,
    pid: Option<u32>,
}

#[derive(Default, Clone, Copy)]
struct VideoPhase {
    /// yt-dlp downloads several files (video, audio): bytes completed in earlier phases.
    base: i64,
    last: i64,
    approx_total: i64,
}

#[derive(Default)]
struct Inner {
    items: Vec<DownloadItem>,
    tasks: HashMap<Uuid, Task>,
    next_generation: u64,
    samples: HashMap<Uuid, (i64, Instant)>,
    video_phase: HashMap<Uuid, VideoPhase>,
    /// YouTube downloads waiting for the deno install prompt / install to finish.
    deno_waiters: Vec<Uuid>,
    deno_prompting: bool,
    deno_declined: bool,
    /// Video downloads retried without cookies after YouTube rejected the signed-in client.
    cookieless: HashSet<Uuid>,
    save_pending: bool,
    ticks: u64,
}

impl Inner {
    fn index(&self, id: Uuid) -> Option<usize> {
        self.items.iter().position(|i| i.id == id)
    }

    fn is_current(&self, id: Uuid, generation: u64) -> bool {
        self.tasks.get(&id).is_some_and(|t| t.generation == generation)
    }

    fn next_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }
}

pub struct Store {
    app: AppHandle,
    settings: Arc<SettingsStore>,
    inner: Mutex<Inner>,
    changed: Notify,
    path: PathBuf,
}

impl Store {
    pub fn new(app: AppHandle, settings: Arc<SettingsStore>) -> Arc<Self> {
        let path = crate::paths::data_dir().join("downloads.json");
        let store = Arc::new(Self { app, settings, inner: Mutex::new(Inner::default()), changed: Notify::new(), path });
        store.load();
        store
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mark_changed(&self) {
        self.changed.notify_one();
    }

    /// Emits the list to the UI (coalesced) and runs the 0.5 s tick (speed, tray, periodic save).
    pub fn start_background(self: &Arc<Self>) {
        let s = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                s.changed.notified().await;
                tokio::time::sleep(Duration::from_millis(120)).await;
                let _ = s.app.emit("items", s.views());
            }
        });
        let s = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(500));
            loop {
                interval.tick().await;
                s.tick();
            }
        });
    }

    // MARK: - Queries

    /// Final folder for an item: the download folder, plus the category subfolder (Compressed, Documents, Music,
    /// Programs, Video) when enabled. Files without a category go to the download folder itself, like IDM.
    fn destination_dir(settings: &Settings, item: &DownloadItem) -> PathBuf {
        let dir = match item.category().folder() {
            Some(folder) if settings.organize_by_category => item.directory.join(folder),
            _ => item.directory.clone(),
        };
        match item.subfolder.as_deref() {
            Some(sub) => dir.join(sub),
            None => dir,
        }
    }

    pub fn views(&self) -> Vec<ItemView> {
        let settings = self.settings.get();
        let inner = self.lock();
        inner.items.iter().map(|i| Self::view(&settings, i)).collect()
    }

    fn view(settings: &Settings, i: &DownloadItem) -> ItemView {
        let destination = i
            .final_path
            .clone()
            .unwrap_or_else(|| Self::destination_dir(settings, i).join(&i.file_name).to_string_lossy().into_owned());
        ItemView {
            id: i.id,
            url: i.url.clone(),
            file_name: i.file_name.clone(),
            kind: i.kind,
            state: i.state,
            total_bytes: i.total_bytes,
            received_bytes: i.received_bytes,
            supports_ranges: i.supports_ranges,
            segments: i.segments.clone(),
            error_message: i.error_message.clone(),
            created_at: i.created_at,
            completed_at: i.completed_at,
            final_path: i.final_path.clone(),
            speed: i.speed,
            stage: i.stage.clone(),
            category: i.category(),
            destination,
        }
    }

    pub fn item_view(&self, id: Uuid) -> Option<ItemView> {
        let settings = self.settings.get();
        let inner = self.lock();
        inner.items.iter().find(|i| i.id == id).map(|i| Self::view(&settings, i))
    }

    pub fn item_log(&self, id: Uuid) -> String {
        self.lock().items.iter().find(|i| i.id == id).map(|i| i.log.clone()).unwrap_or_default()
    }

    pub fn summary(&self) -> Summary {
        let inner = self.lock();
        let active: Vec<&DownloadItem> = inner.items.iter().filter(|i| i.state.is_active()).collect();
        let measurable: Vec<&&DownloadItem> = active.iter().filter(|i| i.total_bytes > 0).collect();
        let total: i64 = measurable.iter().map(|i| i.total_bytes).sum();
        let received: i64 = measurable.iter().map(|i| i.received_bytes.min(i.total_bytes)).sum();
        Summary {
            active_count: active.len(),
            unfinished_count: inner.items.iter().filter(|i| i.state != DownloadState::Completed).count(),
            overall_progress: (total > 0).then(|| received as f64 / total as f64),
            total_speed: inner.items.iter().filter(|i| i.state == DownloadState::Downloading).map(|i| i.speed).sum(),
        }
    }

    /// Creates the download folder now (first run / setup) rather than in the middle of the first download.
    pub fn prepare_download_folder(&self) {
        let _ = std::fs::create_dir_all(self.settings.get().download_dir());
    }

    // MARK: - Adding

    pub fn add(self: &Arc<Self>, req: NewDownload) -> Option<Uuid> {
        let id = Uuid::new_v4();
        let manifest = match req.manifest.as_deref() {
            Some(text) if req.kind == DownloadKind::Video => Some(write_manifest(text, id)?),
            _ => None,
        };
        let url = match manifest {
            Some(u) => u,
            None => Url::parse(&req.url).ok().filter(|u| matches!(u.scheme(), "http" | "https"))?,
        };
        let settings = self.settings.get();
        let mut headers = req.extra_headers.clone();
        if let Some(r) = req.referer.filter(|s| !s.is_empty()) {
            headers.insert("Referer".into(), r);
        }
        if let Some(ua) = req.user_agent.filter(|s| !s.is_empty()) {
            headers.insert("User-Agent".into(), ua);
        }
        if let Some(c) = req.cookie_header.filter(|s| !s.is_empty()) {
            headers.insert("Cookie".into(), c);
        }
        headers.entry("User-Agent".into()).or_insert_with(|| DEFAULT_USER_AGENT.into());

        let mut name = req.file_name.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| file_name_from_url(&url));
        if req.kind == DownloadKind::Video {
            name = url.host_str().unwrap_or("video").to_string();
        }
        let mut item = DownloadItem {
            id,
            url: url.to_string(),
            file_name: sanitize(&name),
            directory: settings.download_dir(),
            kind: req.kind,
            headers,
            requested_segments: req.segments.unwrap_or(0).min(16),
            ytdlp_format: req.format.clone().filter(|f| !f.trim().is_empty()),
            audio_format: req.audio_format.clone().filter(|a| matches!(a.as_str(), "m4a" | "mp3")),
            output_title: req.title.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(sanitize),
            subfolder: req.subfolder.as_deref().map(folder_name).filter(|s| !s.is_empty()),
            ..Default::default()
        };
        if let Some(t) = item.output_title.clone().filter(|_| req.kind == DownloadKind::Video) {
            item.file_name = t;
        }
        if req.kind == DownloadKind::Video {
            item.audio_only = item.audio_format.is_some() || (item.ytdlp_format.is_none() && settings.default_video_is_audio());
        }
        if req.kind == DownloadKind::Video && !req.cookies.is_empty() {
            item.cookie_file = write_cookie_file(&req.cookies, item.id);
        }
        let _ = std::fs::create_dir_all(&item.directory);
        {
            let mut inner = self.lock();
            inner.items.insert(0, item);
            inner.save_pending = true;
            if settings.auto_start {
                self.pump_queue(&mut inner);
            }
        }
        self.mark_changed();
        if settings.show_detail_on_start && !req.quiet {
            windows::show_detail(&self.app, id, false);
        }
        Some(id)
    }

    /// One URL per line; anything that is not an http(s) URL is skipped.
    pub fn add_urls(self: &Arc<Self>, text: &str, kind: DownloadKind, segments: Option<u32>) -> usize {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(|l| self.add(NewDownload { url: l.to_string(), kind, segments, ..Default::default() }))
            .count()
    }

    // MARK: - Queue

    fn pump_queue(self: &Arc<Self>, inner: &mut Inner) {
        let max = self.settings.get().max_concurrent as usize;
        loop {
            let active = inner.items.iter().filter(|i| i.state.is_active()).count();
            if active >= max {
                break;
            }
            // Oldest queued item first (the list is newest-first).
            let Some(idx) = inner.items.iter().rposition(|i| i.state == DownloadState::Queued) else { break };
            match inner.items[idx].kind {
                DownloadKind::File => self.start_file(inner, idx),
                DownloadKind::Video => self.start_video(inner, idx),
            }
        }
    }

    fn start_file(self: &Arc<Self>, inner: &mut Inner, idx: usize) {
        let settings = self.settings.get();
        let item = inner.items[idx].clone();
        let id = item.id;
        let Ok(url) = Url::parse(&item.url) else {
            let it = &mut inner.items[idx];
            it.state = DownloadState::Failed;
            it.error_message = Some(t("Invalid URL"));
            return;
        };
        let part = item.part_path();
        let resumable = item.supports_ranges && item.total_bytes > 0 && !item.segments.is_empty() && part.exists();
        let count = if item.requested_segments > 0 { item.requested_segments } else { settings.segment_count };
        let headers = item.headers.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let mut cfg = segmented::Config::new(url, headers, part, count as usize);
        if resumable {
            cfg.resume_segments = item.segments.clone();
            cfg.resume_total = item.total_bytes;
        }
        let generation = inner.next_generation();
        let cancel = CancellationToken::new();
        inner.tasks.insert(id, Task { generation, cancel: Some(cancel.clone()), pid: None });
        {
            let it = &mut inner.items[idx];
            it.state = if resumable { DownloadState::Downloading } else { DownloadState::Probing };
            it.error_message = None;
            it.speed = 0.0;
            if !resumable {
                it.received_bytes = 0;
                it.segments.clear();
            }
        }
        let (s1, s2) = (self.clone(), self.clone());
        tauri::async_runtime::spawn(async move {
            let result = segmented::download(cfg, cancel, move |ev| s1.on_file_event(id, generation, ev)).await;
            s2.on_file_finished(id, generation, result);
        });
    }

    fn on_file_event(&self, id: Uuid, generation: u64, ev: Event) {
        {
            let mut inner = self.lock();
            if !inner.is_current(id, generation) {
                return;
            }
            let Some(idx) = inner.index(id) else { return };
            let item = &mut inner.items[idx];
            match ev {
                Event::Probe(p) => Self::apply_probe(item, p),
                Event::Progress(received, segments) => {
                    item.received_bytes = received;
                    item.segments = segments;
                    if item.state == DownloadState::Probing {
                        item.state = DownloadState::Downloading;
                    }
                }
            }
        }
        self.mark_changed();
    }

    fn apply_probe(item: &mut DownloadItem, p: ProbeResult) {
        item.total_bytes = p.total_bytes;
        item.supports_ranges = p.supports_ranges;
        if item.state == DownloadState::Probing {
            item.state = DownloadState::Downloading;
        }
        let original = Url::parse(&item.url).ok();
        if let Some(n) = p.suggested_file_name.filter(|n| !n.trim().is_empty()) {
            item.file_name = sanitize(&n);
        } else if let (Some(orig), Some(fin)) = (&original, p.final_url.as_deref().and_then(|u| Url::parse(u).ok())) {
            if &fin != orig && item.file_name == sanitize(&file_name_from_url(orig)) {
                let candidate = file_name_from_url(&fin);
                if !extension(&candidate).is_empty() {
                    item.file_name = sanitize(&candidate);
                }
            }
        }
        if extension(&item.file_name).is_empty() {
            if let Some(ext) = p.content_type.as_deref().and_then(extension_for_mime) {
                item.file_name.push('.');
                item.file_name.push_str(ext);
            }
        }
    }

    fn on_file_finished(self: &Arc<Self>, id: Uuid, generation: u64, result: Result<(), DlError>) {
        let settings = self.settings.get();
        let mut completed: Option<DownloadItem> = None;
        {
            let mut inner = self.lock();
            if !inner.is_current(id, generation) {
                return;
            }
            inner.tasks.remove(&id);
            let Some(idx) = inner.index(id) else { return };
            match result {
                Ok(()) => {
                    let item = &mut inner.items[idx];
                    let dest_dir = Self::destination_dir(&settings, item);
                    let dest = unique_destination(&dest_dir.join(&item.file_name));
                    let moved = std::fs::create_dir_all(&dest_dir).and_then(|_| move_file(&item.part_path(), &dest));
                    match moved {
                        Ok(()) => {
                            apply_mark_of_the_web(&dest, &item.url);
                            item.file_name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            item.final_path = Some(dest.to_string_lossy().into_owned());
                            item.state = DownloadState::Completed;
                            item.completed_at = Some(util::now_ms());
                            item.speed = 0.0;
                            if item.total_bytes <= 0 {
                                item.total_bytes = item.received_bytes;
                            }
                            completed = Some(item.clone());
                        }
                        Err(e) => {
                            item.state = DownloadState::Failed;
                            item.error_message = Some(e.to_string());
                        }
                    }
                }
                Err(DlError::Cancelled) => {}
                Err(DlError::Failed(msg)) => {
                    let item = &mut inner.items[idx];
                    if !matches!(item.state, DownloadState::Paused | DownloadState::Cancelled) {
                        item.state = DownloadState::Failed;
                        item.error_message = Some(msg);
                        item.speed = 0.0;
                    }
                }
            }
            inner.save_pending = true;
            self.pump_queue(&mut inner);
        }
        self.mark_changed();
        if let Some(item) = completed {
            self.notify_completed(&item);
        }
    }

    // MARK: - Video (yt-dlp)

    fn start_video(self: &Arc<Self>, inner: &mut Inner, idx: usize) {
        let settings = self.settings.get();
        let item = inner.items[idx].clone();
        let id = item.id;
        let Some(exe) = video::locate_ytdlp(&settings.ytdlp_path) else {
            let it = &mut inner.items[idx];
            it.state = DownloadState::Failed;
            it.error_message = Some(t("yt-dlp not found. Use “Install yt-dlp” in Settings ▸ Video."));
            return;
        };
        let host = item.host();
        if video::is_youtube(host.as_deref()) && video::locate_deno().is_none() && !settings.deno_prompt_suppressed && !inner.deno_declined {
            let it = &mut inner.items[idx];
            it.state = DownloadState::Downloading;
            it.error_message = None;
            it.speed = 0.0;
            it.stage = t("Waiting for the JavaScript runtime (deno)…");
            if !inner.deno_waiters.contains(&id) {
                inner.deno_waiters.push(id);
            }
            if !inner.deno_prompting {
                inner.deno_prompting = true;
                let s = self.clone();
                tauri::async_runtime::spawn(async move { s.prompt_for_deno().await });
            }
            return;
        }
        let use_cookies = !inner.cookieless.contains(&id);
        {
            let it = &mut inner.items[idx];
            it.state = DownloadState::Downloading;
            it.error_message = None;
            it.received_bytes = 0;
            it.speed = 0.0;
            it.total_bytes = -1;
            it.stage = t("Fetching video information…");
            if use_cookies {
                // A cookieless retry keeps the failed attempt's output visible above it.
                it.log.clear();
            }
        }
        let out_dir = Self::destination_dir(&settings, &item);
        let _ = std::fs::create_dir_all(&out_dir);
        let generation = inner.next_generation();
        inner.tasks.insert(id, Task { generation, cancel: None, pid: None });
        inner.video_phase.insert(id, VideoPhase { approx_total: -1, ..Default::default() });

        let mut format_args = settings.ytdlp_arguments_for(item.ytdlp_format.as_deref(), item.audio_format.as_deref());
        if item.url.starts_with("file:") {
            // A playlist saved by `write_manifest`.
            format_args.push("--enable-file-urls".into());
        }
        let s = self.clone();
        tauri::async_runtime::spawn(async move {
            let (s1, s2) = (s.clone(), s.clone());
            let started = video::run(
                RunOptions {
                    executable: &exe,
                    url: &item.url,
                    output_dir: &out_dir,
                    output_name: item.output_title.as_deref(),
                    format_args,
                    headers: &item.headers,
                    cookie_file: item.cookie_file.as_deref(),
                    use_cookies,
                },
                move |ev| s1.on_video_event(id, generation, ev),
                move |res| s2.on_video_finished(id, generation, res),
            );
            let mut inner = s.lock();
            match started {
                Ok(pid) => {
                    if let Some(task) = inner.tasks.get_mut(&id).filter(|t| t.generation == generation) {
                        task.pid = Some(pid);
                    } else {
                        // Paused or cancelled while starting.
                        drop(inner);
                        util::kill_tree(pid);
                    }
                }
                Err(e) => {
                    if inner.is_current(id, generation) {
                        inner.tasks.remove(&id);
                        if let Some(idx) = inner.index(id) {
                            let it = &mut inner.items[idx];
                            it.state = DownloadState::Failed;
                            it.error_message = Some(e);
                        }
                        s.pump_queue(&mut inner);
                        drop(inner);
                        s.mark_changed();
                    }
                }
            }
        });
    }

    fn on_video_event(&self, id: Uuid, generation: u64, ev: VideoEvent) {
        static FORMAT_SUFFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.*)\.f[0-9]+[-a-z0-9]*(\.[a-z0-9]+)$").unwrap());
        {
            let mut inner = self.lock();
            if !inner.is_current(id, generation) {
                return;
            }
            let Some(idx) = inner.index(id) else { return };
            let mut phase = inner.video_phase.get(&id).copied().unwrap_or(VideoPhase { approx_total: -1, ..Default::default() });
            let item = &mut inner.items[idx];
            match ev {
                VideoEvent::Progress(p) => {
                    if p.downloaded < phase.last {
                        phase.base += phase.last; // a new file started
                    }
                    phase.last = p.downloaded;
                    let received = phase.base + p.downloaded;
                    item.received_bytes = received;
                    let phase_total = if p.total > 0 { phase.base + p.total } else { -1 };
                    item.total_bytes = phase.approx_total.max(phase_total).max(received);
                    item.speed = p.speed;
                }
                VideoEvent::Line(line) => {
                    item.log.push_str(&line);
                    item.log.push('\n');
                    if item.log.len() > 6000 {
                        let mut cut = item.log.len() - 6000;
                        while !item.log.is_char_boundary(cut) {
                            cut += 1;
                        }
                        item.log.drain(..cut);
                    }
                    if let Some(stage) = video::stage_for(&line) {
                        item.stage = stage;
                    }
                    if let Some(n) = video::approx_size(&line) {
                        phase.approx_total = n;
                        if item.total_bytes < n {
                            item.total_bytes = n;
                        }
                    }
                    if let Some((_, path)) = line.split_once("Destination: ") {
                        let path = path.trim();
                        // Show the final title; strip yt-dlp's intermediate ".f137" format suffix.
                        if let Some(name) = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()) {
                            item.file_name = FORMAT_SUFFIX.replace(&name, "$1$2").into_owned();
                        }
                        if Path::new(path).is_absolute() && !item.temp_paths.iter().any(|p| p == path) {
                            item.temp_paths.push(path.to_string());
                        }
                    }
                    if let Some(rest) = line.strip_prefix("[Merger] Merging formats into \"") {
                        if let Some(path) = rest.strip_suffix('"') {
                            if let Some(name) = Path::new(path).file_name() {
                                item.file_name = name.to_string_lossy().into_owned();
                            }
                            if !item.temp_paths.iter().any(|p| p == path) {
                                item.temp_paths.push(path.to_string());
                            }
                        }
                    }
                }
            }
            inner.video_phase.insert(id, phase);
        }
        self.mark_changed();
    }

    fn on_video_finished(self: &Arc<Self>, id: Uuid, generation: u64, result: Result<Option<String>, String>) {
        let mut completed: Option<DownloadItem> = None;
        {
            let mut inner = self.lock();
            if !inner.is_current(id, generation) {
                return;
            }
            inner.tasks.remove(&id);
            inner.video_phase.remove(&id);
            let Some(idx) = inner.index(id) else { return };
            if inner.items[idx].state != DownloadState::Downloading {
                return; // paused or cancelled by the user
            }
            match result {
                Ok(path) => {
                    let item = &mut inner.items[idx];
                    item.state = DownloadState::Completed;
                    item.completed_at = Some(util::now_ms());
                    item.speed = 0.0;
                    // yt-dlp removed its intermediate files itself.
                    item.temp_paths.clear();
                    if let Some(path) = path {
                        let p = PathBuf::from(&path);
                        apply_mark_of_the_web(&p, &item.url);
                        item.file_name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        if let Ok(meta) = std::fs::metadata(&p) {
                            item.total_bytes = meta.len() as i64;
                            item.received_bytes = meta.len() as i64;
                        }
                        item.final_path = Some(path);
                    } else if item.total_bytes > 0 {
                        item.received_bytes = item.total_bytes;
                    }
                    completed = Some(item.clone());
                }
                Err(message) => {
                    let item = &inner.items[idx];
                    let host = item.host();
                    let retry_without_cookies = !inner.cookieless.contains(&id)
                        && video::is_youtube(host.as_deref())
                        && video::is_signed_in_client_failure(&message)
                        && video::has_cookies(item.cookie_file.as_deref(), host.as_deref());
                    if retry_without_cookies {
                        inner.cookieless.insert(id);
                        let item = &mut inner.items[idx];
                        item.log.push_str(&t("→ YouTube rejected the signed-in request; retrying without cookies…"));
                        item.log.push('\n');
                        self.start_video(&mut inner, idx);
                        drop(inner);
                        self.mark_changed();
                        return;
                    }
                    let item = &mut inner.items[idx];
                    item.state = DownloadState::Failed;
                    item.error_message = Some(message);
                    item.speed = 0.0;
                }
            }
            inner.save_pending = true;
            self.pump_queue(&mut inner);
        }
        self.mark_changed();
        if let Some(item) = completed {
            self.notify_completed(&item);
        }
    }

    /// Asks once whether to install deno (needed by yt-dlp for YouTube), then starts the waiting downloads.
    async fn prompt_for_deno(self: Arc<Self>) {
        let spec = PromptSpec {
            title: t("Install the JavaScript runtime for YouTube?"),
            message: tf!(
                "YouTube now requires yt-dlp to run JavaScript (deno) to unlock video streams. Without it, YouTube downloads may fail with errors such as “The page needs to be reloaded” or “n challenge solving failed”.\n\nEDM can download deno ({0}) from its official GitHub releases and keep it in its own data folder.",
                tools::DENO_DOWNLOAD_SIZE
            ),
            buttons: vec![t("Install deno"), t("Not Now")],
            default_button: 0,
            cancel_button: Some(1),
            checkbox: Some(t("Don’t ask again")),
            ..Default::default()
        };
        let answer = prompt::ask(&self.app, spec).await;
        if answer.checked {
            self.settings.update(&self.app, |s| s.deno_prompt_suppressed = true);
        }
        if answer.button != Some(0) {
            let mut inner = self.lock();
            inner.deno_declined = true;
            self.resume_deno_waiters(&mut inner);
            drop(inner);
            self.mark_changed();
            return;
        }
        {
            let mut inner = self.lock();
            for id in inner.deno_waiters.clone() {
                if let Some(idx) = inner.index(id) {
                    inner.items[idx].stage = t("Installing deno…");
                }
            }
        }
        self.mark_changed();
        let installer = self.app.state::<Arc<tools::Tools>>().inner().clone();
        let result = installer.install_deno().await;
        let mut inner = self.lock();
        if let Err(e) = result {
            // Don't loop on a failing install; Settings ▸ Video can retry.
            inner.deno_declined = true;
            for id in inner.deno_waiters.clone() {
                if let Some(idx) = inner.index(id) {
                    let line = tf!("deno could not be installed: {0}", e);
                    inner.items[idx].log.push_str(&format!("{line}\n"));
                }
            }
        }
        self.resume_deno_waiters(&mut inner);
        drop(inner);
        self.mark_changed();
    }

    fn resume_deno_waiters(self: &Arc<Self>, inner: &mut Inner) {
        inner.deno_prompting = false;
        for id in std::mem::take(&mut inner.deno_waiters) {
            if let Some(idx) = inner.index(id).filter(|&i| inner.items[i].state == DownloadState::Downloading) {
                self.start_video(inner, idx);
            }
        }
    }

    // MARK: - User actions

    /// Stops the running task of an item (keeps its entry so the final segment state is still accepted).
    fn stop_task(inner: &Inner, id: Uuid) -> Option<u32> {
        let task = inner.tasks.get(&id)?;
        if let Some(c) = &task.cancel {
            c.cancel();
        }
        task.pid
    }

    fn kill_later(pid: Option<u32>) {
        if let Some(pid) = pid {
            tauri::async_runtime::spawn_blocking(move || util::kill_tree(pid));
        }
    }

    pub fn pause(self: &Arc<Self>, id: Uuid) {
        let pid = {
            let mut inner = self.lock();
            let Some(idx) = inner.index(id) else { return };
            let st = inner.items[idx].state;
            if !(st.is_active() || st == DownloadState::Queued) {
                return;
            }
            inner.items[idx].state = DownloadState::Paused;
            inner.items[idx].speed = 0.0;
            let pid = Self::stop_task(&inner, id);
            inner.save_pending = true;
            self.pump_queue(&mut inner);
            pid
        };
        Self::kill_later(pid);
        self.mark_changed();
    }

    pub fn resume(self: &Arc<Self>, id: Uuid) {
        {
            let mut inner = self.lock();
            let Some(idx) = inner.index(id) else { return };
            if !matches!(inner.items[idx].state, DownloadState::Paused | DownloadState::Failed | DownloadState::Cancelled) {
                return;
            }
            inner.items[idx].state = DownloadState::Queued;
            inner.items[idx].error_message = None;
            inner.cookieless.remove(&id);
            self.pump_queue(&mut inner);
        }
        self.mark_changed();
    }

    /// Cancels the download and deletes everything it wrote so far (part file, yt-dlp fragments). The item stays
    /// in the list as "Cancelled"; restarting it starts over.
    pub fn cancel(self: &Arc<Self>, id: Uuid) {
        let (pid, leftovers) = {
            let mut inner = self.lock();
            let Some(idx) = inner.index(id) else { return };
            let pid = Self::stop_task(&inner, id);
            inner.tasks.remove(&id);
            let leftovers = leftover_files(&inner.items[idx], &completed_paths(&inner.items));
            let item = &mut inner.items[idx];
            item.state = DownloadState::Cancelled;
            item.speed = 0.0;
            item.received_bytes = 0;
            item.segments.clear();
            item.temp_paths.clear();
            item.stage.clear();
            inner.save_pending = true;
            self.pump_queue(&mut inner);
            (pid, leftovers)
        };
        Self::kill_later(pid);
        // The task (or yt-dlp) may hold the files for a moment; delete them once it has let go,
        // unless the download was restarted in the meantime.
        let s = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let still_cancelled = s.lock().items.iter().any(|i| i.id == id && i.state == DownloadState::Cancelled);
            if still_cancelled {
                delete_files(leftovers).await;
            }
        });
        self.mark_changed();
    }

    pub fn remove(self: &Arc<Self>, id: Uuid, delete_file: bool) {
        let (pid, item, leftovers) = {
            let mut inner = self.lock();
            let Some(idx) = inner.index(id) else { return };
            let pid = Self::stop_task(&inner, id);
            inner.tasks.remove(&id);
            let leftovers = leftover_files(&inner.items[idx], &completed_paths(&inner.items));
            let item = inner.items.remove(idx);
            inner.samples.remove(&id);
            inner.save_pending = true;
            self.pump_queue(&mut inner);
            (pid, item, leftovers)
        };
        Self::kill_later(pid);
        windows::close_detail(&self.app, id);
        if delete_file {
            if let Some(fp) = item.final_path.as_deref().map(Path::new).filter(|p| p.exists()) {
                // Recycle Bin, so the user can recover the file.
                let _ = trash::delete(fp);
            }
        }
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let mut files = leftovers;
            files.extend(item.cookie_file.map(PathBuf::from));
            files.extend(manifest_path(&item.url));
            delete_files(files).await;
        });
        self.mark_changed();
    }

    fn ids_where(&self, f: impl Fn(&DownloadItem) -> bool) -> Vec<Uuid> {
        self.lock().items.iter().filter(|i| f(i)).map(|i| i.id).collect()
    }

    pub fn pause_all(self: &Arc<Self>) {
        for id in self.ids_where(|i| i.state.is_active() || i.state == DownloadState::Queued) {
            self.pause(id);
        }
    }

    pub fn resume_all(self: &Arc<Self>) {
        for id in self.ids_where(|i| matches!(i.state, DownloadState::Paused | DownloadState::Failed)) {
            self.resume(id);
        }
    }

    pub fn clear_finished(self: &Arc<Self>) {
        for id in self.ids_where(|i| matches!(i.state, DownloadState::Completed | DownloadState::Cancelled)) {
            self.remove(id, false);
        }
    }

    pub fn open(&self, id: Uuid) {
        let path = self.lock().items.iter().find(|i| i.id == id).and_then(|i| i.final_path.clone());
        if let Some(p) = path {
            util::open_path(Path::new(&p));
        }
    }

    pub fn reveal(&self, id: Uuid) {
        let settings = self.settings.get();
        let target = {
            let inner = self.lock();
            let Some(item) = inner.items.iter().find(|i| i.id == id) else { return };
            match item.final_path.as_deref().map(PathBuf::from).filter(|p| p.exists()) {
                Some(p) => p,
                None => Self::destination_dir(&settings, item),
            }
        };
        util::reveal_in_explorer(&target);
    }

    pub fn url_of(&self, id: Uuid) -> Option<String> {
        self.lock().items.iter().find(|i| i.id == id).map(|i| i.url.clone())
    }

    /// Pauses everything and saves synchronously (app exit).
    pub fn shutdown(self: &Arc<Self>) {
        let pids: Vec<u32> = {
            let mut inner = self.lock();
            for item in inner.items.iter_mut() {
                if item.state.is_active() || item.state == DownloadState::Queued {
                    item.state = DownloadState::Paused;
                    item.speed = 0.0;
                }
            }
            inner.tasks.values().filter_map(|t| {
                if let Some(c) = &t.cancel {
                    c.cancel();
                }
                t.pid
            }).collect()
        };
        for pid in pids {
            util::kill_tree(pid);
        }
        self.save();
    }

    // MARK: - Speed, tray, periodic save

    fn tick(self: &Arc<Self>) {
        let now = Instant::now();
        let save_now;
        let mut speed_changed = false;
        let mut stale_files: Vec<PathBuf> = vec![];
        let mut purged = false;
        {
            let mut inner = self.lock();
            let mut active = HashSet::new();
            let inner = &mut *inner;
            for item in inner.items.iter_mut().filter(|i| i.state == DownloadState::Downloading && i.kind == DownloadKind::File) {
                active.insert(item.id);
                if let Some((bytes, time)) = inner.samples.get(&item.id) {
                    let dt = now.duration_since(*time).as_secs_f64();
                    if dt > 0.0 {
                        let inst = ((item.received_bytes - bytes) as f64 / dt).max(0.0);
                        let new = if item.speed == 0.0 { inst } else { item.speed * 0.6 + inst * 0.4 };
                        speed_changed |= (new - item.speed).abs() > 1.0;
                        item.speed = new;
                    }
                }
                inner.samples.insert(item.id, (item.received_bytes, now));
            }
            inner.samples.retain(|id, _| active.contains(id));
            inner.ticks += 1;

            // Paused/failed downloads remember since when; their partial data goes after 24 hours (checked
            // shortly after start and every 10 minutes). The item stays in the list and restarts from zero.
            let now_ms = util::now_ms();
            for item in inner.items.iter_mut() {
                let stalled = matches!(item.state, DownloadState::Paused | DownloadState::Failed);
                if stalled && item.stalled_since.is_none() {
                    item.stalled_since = Some(now_ms);
                    inner.save_pending = true;
                } else if !stalled && item.stalled_since.is_some() {
                    item.stalled_since = None;
                }
            }
            if inner.ticks == 4 || inner.ticks % 1200 == 0 {
                let keep = completed_paths(&inner.items);
                for item in inner.items.iter_mut() {
                    let stale = item.stalled_since.is_some_and(|t| now_ms - t > STALE_PARTIAL_MS);
                    let has_partial = item.received_bytes > 0 || !item.segments.is_empty() || !item.temp_paths.is_empty() || item.part_path().exists();
                    if stale && has_partial {
                        stale_files.extend(leftover_files(item, &keep));
                        item.received_bytes = 0;
                        item.segments.clear();
                        item.temp_paths.clear();
                        inner.save_pending = true;
                        purged = true;
                    }
                }
            }
            let any_active = inner.items.iter().any(|i| i.state.is_active());
            // Segment state goes to disk every 5 s while something downloads.
            save_now = inner.save_pending || (any_active && inner.ticks % 10 == 0);
            inner.save_pending = false;
        }
        if save_now {
            self.save();
        }
        if !stale_files.is_empty() {
            tauri::async_runtime::spawn(delete_files(stale_files));
        }
        if speed_changed || purged {
            self.mark_changed();
        }
        tray::update(&self.app, &self.summary());
    }

    // MARK: - Persistence

    fn load(&self) {
        let Ok(data) = std::fs::read(&self.path) else { return };
        let Ok(mut items) = serde_json::from_slice::<Vec<DownloadItem>>(&data) else { return };
        for item in items.iter_mut() {
            if item.state.is_active() || item.state == DownloadState::Queued {
                item.state = DownloadState::Paused;
            }
        }
        let known: HashSet<String> = items.iter().map(|i| format!(".dm-{}.part", i.id)).collect();
        self.lock().items = items;
        // Remove `.dm-<id>.part` files left behind by downloads that are no longer in the list.
        if let Ok(entries) = std::fs::read_dir(self.settings.get().download_dir()) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with(".dm-") && name.ends_with(".part") && !known.contains(&name) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }

    fn save(&self) {
        let data = {
            let inner = self.lock();
            serde_json::to_vec(&inner.items)
        };
        if let Ok(data) = data {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, data).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    // MARK: - Notification

    fn notify_completed(&self, item: &DownloadItem) {
        if !self.settings.get().notify_on_complete {
            return;
        }
        let _ = self.app.notification().builder().title(t("Download completed")).body(&item.file_name).show();
    }
}

pub fn write_cookie_file(cookies: &[ExtensionCookie], id: Uuid) -> Option<String> {
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    for c in cookies {
        let path = c.path.clone().unwrap_or_else(|| "/".into());
        let secure = if c.secure.unwrap_or(false) { "TRUE" } else { "FALSE" };
        let exp = c.expiration_date.unwrap_or(0.0) as i64;
        let flag = if c.domain.starts_with('.') { "TRUE" } else { "FALSE" };
        lines.push([c.domain.as_str(), flag, &path, secure, &exp.to_string(), &c.name, &c.value].join("\t"));
    }
    let dir = crate::paths::cookies_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{id}.txt"));
    std::fs::write(&path, lines.join("\n") + "\n").ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Saves an HLS/DASH playlist the page built in JavaScript and returns its file:// URL for yt-dlp.
/// Its segment URLs are absolute (a blob: playlist has no base to resolve relative ones against).
fn write_manifest(text: &str, id: Uuid) -> Option<Url> {
    let head = text.trim_start_matches('\u{feff}').trim_start();
    let ext = if head.starts_with("#EXTM3U") {
        "m3u8"
    } else if head.starts_with('<') && head.chars().take(2048).collect::<String>().contains("<MPD") {
        "mpd"
    } else {
        return None;
    };
    if text.len() > 8 * 1024 * 1024 {
        return None;
    }
    let dir = crate::paths::manifests_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{id}.{ext}"));
    std::fs::write(&path, text).ok()?;
    Url::from_file_path(&path).ok()
}

/// The saved playlist behind a download's file:// URL, if it is one of ours.
fn manifest_path(url: &str) -> Option<PathBuf> {
    let path = Url::parse(url).ok().filter(|u| u.scheme() == "file")?.to_file_path().ok()?;
    path.starts_with(crate::paths::manifests_dir()).then_some(path)
}

/// Tags a downloaded file like a browser download would (Zone.Identifier stream, "Internet" zone),
/// so SmartScreen and Office Protected View assess it on first open.
pub fn apply_mark_of_the_web(file: &Path, source: &str) {
    let mut ads = file.as_os_str().to_owned();
    ads.push(":Zone.Identifier");
    let _ = std::fs::write(PathBuf::from(ads), format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={source}\r\n"));
}

/// Final files of completed downloads: never deleted as somebody else's leftovers.
fn completed_paths(items: &[DownloadItem]) -> HashSet<PathBuf> {
    items.iter().filter_map(|i| i.final_path.as_deref().map(PathBuf::from)).collect()
}

/// Everything an unfinished download has written: the part file, and for yt-dlp the announced files plus their
/// working files next to them (`.part`, `.part-Frag*`, `.ytdl`, `.temp.*`).
fn leftover_files(item: &DownloadItem, keep: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut out = vec![item.part_path()];
    for announced in item.temp_paths.iter().map(PathBuf::from) {
        if !keep.contains(&announced) {
            out.push(announced.clone());
        }
        let (Some(dir), Some(name), Some(stem)) = (announced.parent(), announced.file_name(), announced.file_stem()) else { continue };
        let (name, stem) = (name.to_string_lossy().into_owned(), format!("{}.", stem.to_string_lossy()));
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let working = n.ends_with(".part") || n.contains(".part-Frag") || n.ends_with(".ytdl") || n.contains(".temp.") || n.ends_with(".temp");
            if working && (n.starts_with(&name) || n.starts_with(&stem)) && !keep.contains(&e.path()) {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Deletes files, retrying briefly while a just-killed process still holds them.
async fn delete_files(files: Vec<PathBuf>) {
    let mut pending: Vec<PathBuf> = files.into_iter().filter(|p| p.exists()).collect();
    for _ in 0..8 {
        pending.retain(|p| std::fs::remove_file(p).is_err() && p.exists());
        if pending.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        // Different volume (e.g. the category folder is a junction to another drive).
        Err(_) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
    }
}

// MARK: - File name helpers

pub fn file_name_from_url(url: &Url) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let name = percent_encoding::percent_decode_str(last).decode_utf8_lossy().into_owned();
    if name.is_empty() {
        url.host_str().unwrap_or("download").to_string()
    } else {
        name
    }
}

/// Makes a name safe for Windows: no reserved characters or device names, no trailing dots/spaces.
/// A folder name from a title (playlist folders), kept short so paths stay under the Windows path limit.
pub fn folder_name(s: &str) -> String {
    let short: String = s.trim().chars().take(80).collect();
    let n = sanitize(&short);
    let n = n.trim_end_matches(['.', ' ']);
    if n.is_empty() { "Playlist".into() } else { n.to_string() }
}

pub fn sanitize(s: &str) -> String {
    let mut n: String = s
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    n = n.trim().trim_end_matches(['.', ' ']).to_string();
    if n.starts_with('.') {
        n.insert(0, '_');
    }
    let stem = n.split('.').next().unwrap_or("").to_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3",
        "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        n.insert(0, '_');
    }
    if n.chars().count() > 200 {
        let ext = extension(&n);
        let base: String = Path::new(&n).file_stem().map(|s| s.to_string_lossy().chars().take(180).collect()).unwrap_or_default();
        n = if ext.is_empty() { base } else { format!("{base}.{ext}") };
    }
    if n.is_empty() {
        "download".into()
    } else {
        n
    }
}

pub fn unique_destination(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default();
    let base = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    (2..)
        .map(|n| dir.join(if ext.is_empty() { format!("{base} ({n})") } else { format!("{base} ({n}).{ext}") }))
        .find(|c| !c.exists())
        .unwrap()
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "application/pdf" => "pdf",
        "application/zip" | "application/x-zip-compressed" => "zip",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" | "application/x-rar-compressed" => "rar",
        "application/gzip" | "application/x-gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-msdownload" | "application/vnd.microsoft.portable-executable" | "application/x-msdos-program" => "exe",
        "application/x-msi" | "application/x-ms-installer" => "msi",
        "application/x-iso9660-image" => "iso",
        "application/vnd.android.package-archive" => "apk",
        "application/java-archive" => "jar",
        "application/epub+zip" => "epub",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        "application/json" => "json",
        "text/plain" => "txt",
        "text/csv" => "csv",
        "text/html" => "html",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-matroska" => "mkv",
        "video/quicktime" => "mov",
        "video/x-msvideo" => "avi",
        "audio/mpeg" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/flac" => "flac",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_page_built_playlists() {
        let dir = std::env::temp_dir().join(format!("edm-test-{}", Uuid::new_v4()));
        std::env::set_var("EDM_DATA_DIR", &dir);
        let id = Uuid::new_v4();
        let url = write_manifest("\u{feff}#EXTM3U\n#EXTINF:4,\nhttps://cdn.example/s1.ts\n#EXT-X-ENDLIST\n", id).unwrap();
        assert_eq!(url.scheme(), "file");
        let path = manifest_path(url.as_str()).unwrap();
        assert!(path.ends_with(format!("{id}.m3u8")) && path.is_file());
        let dash = write_manifest("<?xml version=\"1.0\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"></MPD>", id).unwrap();
        assert!(dash.as_str().ends_with(".mpd"));
        assert!(write_manifest("<html>not a playlist</html>", id).is_none());
        // Only our own playlists count: never delete a file a user pointed a download at.
        assert!(manifest_path("file:///C:/Users/Public/movie.m3u8").is_none());
        assert!(manifest_path("https://cdn.example/a.m3u8").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizes_windows_names() {
        assert_eq!(sanitize("a/b:c?.zip"), "a_b_c_.zip");
        assert_eq!(sanitize("CON.txt"), "_CON.txt");
        assert_eq!(sanitize(".hidden"), "_.hidden");
        assert_eq!(sanitize("name. "), "name");
        assert_eq!(sanitize(""), "download");
    }

    #[test]
    fn leftovers_of_a_cancelled_video() {
        let dir = std::env::temp_dir().join(format!("edm-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let touch = |n: &str| std::fs::write(dir.join(n), b"x").unwrap();
        for n in ["Clip [a1].f137.mp4.part", "Clip [a1].f137.mp4.part-Frag3", "Clip [a1].f137.mp4.ytdl", "Clip [a1].f140.m4a", "Clip [a1].temp.mp4", "Other.mp4", "Clip [a1].mp4"] {
            touch(n);
        }
        let item = DownloadItem {
            directory: dir.clone(),
            temp_paths: vec![dir.join("Clip [a1].f137.mp4").to_string_lossy().into(), dir.join("Clip [a1].f140.m4a").to_string_lossy().into(), dir.join("Clip [a1].mp4").to_string_lossy().into()],
            ..Default::default()
        };
        // "Clip [a1].mp4" is the finished file of another (completed) download: it must survive.
        let keep: HashSet<PathBuf> = [dir.join("Clip [a1].mp4")].into_iter().collect();
        let files = leftover_files(&item, &keep);
        let names: HashSet<String> = files.iter().filter(|p| p.exists()).map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
        let expected: HashSet<String> = ["Clip [a1].f137.mp4.part", "Clip [a1].f137.mp4.part-Frag3", "Clip [a1].f137.mp4.ytdl", "Clip [a1].f140.m4a", "Clip [a1].temp.mp4"]
            .iter().map(|s| s.to_string()).collect();
        assert_eq!(names, expected);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn url_names() {
        let u = Url::parse("https://example.com/files/%C3%A7ay%20list.pdf?x=1").unwrap();
        assert_eq!(file_name_from_url(&u), "çay list.pdf");
        assert_eq!(file_name_from_url(&Url::parse("https://example.com/").unwrap()), "example.com");
    }
}
