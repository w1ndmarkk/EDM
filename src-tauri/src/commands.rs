//! Commands invoked by the web UI. They are async so they run off the main thread (some touch windows,
//! the file system or child processes).

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, LogicalSize, Manager, State, WebviewWindow};
use tauri_plugin_autostart::ManagerExt;
use uuid::Uuid;

use crate::browsers::{self, Browser, BrowserInfo};
use crate::clipboard;
use crate::models::{DownloadKind, ItemView};
use crate::prompt::{self, PromptSpec};
use crate::server::{Server, ServerStatus, VERSION};
use crate::settings::{Settings, SettingsStore};
use crate::sitelogin::{self, LoginStatus};
use crate::store::{NewDownload, Store, Summary};
use crate::tools::{Tools, ToolsStatus};
use crate::updater::{Updater, UpdaterStatus};
use crate::{i18n, paths, util, video, windows};

type Res<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    lang: &'static str,
    version: &'static str,
    settings: Settings,
    items: Vec<ItemView>,
    summary: Summary,
    server: ServerStatus,
    default_capture_types: &'static str,
    default_excluded_sites: &'static str,
    default_download_dir: String,
}

#[tauri::command]
pub async fn bootstrap(settings: State<'_, Arc<SettingsStore>>, store: State<'_, Arc<Store>>, server: State<'_, Arc<Server>>) -> Res<Bootstrap> {
    Ok(Bootstrap {
        lang: i18n::lang(),
        version: VERSION,
        settings: settings.get(),
        items: store.views(),
        summary: store.summary(),
        server: server.status(),
        default_capture_types: crate::settings::DEFAULT_CAPTURE_TYPES,
        default_excluded_sites: crate::settings::DEFAULT_EXCLUDED_SITES,
        default_download_dir: paths::default_download_dir().to_string_lossy().into_owned(),
    })
}

// MARK: - Downloads

#[tauri::command]
pub async fn get_item(store: State<'_, Arc<Store>>, id: Uuid) -> Res<Option<ItemView>> {
    Ok(store.item_view(id))
}

#[tauri::command]
pub async fn get_item_log(store: State<'_, Arc<Store>>, id: Uuid) -> Res<String> {
    Ok(store.item_log(id))
}

#[tauri::command]
pub async fn add_urls(store: State<'_, Arc<Store>>, text: String, video: bool, segments: Option<u32>) -> Res<usize> {
    let kind = if video { DownloadKind::Video } else { DownloadKind::File };
    Ok(store.add_urls(&text, kind, segments.filter(|s| *s > 0)))
}

// MARK: - Playlists

/// Lists a playlist's videos (YouTube and the other sites yt-dlp knows). Retries without the site-login cookies
/// when they make YouTube refuse.
#[tauri::command]
pub async fn fetch_playlist(settings: State<'_, Arc<SettingsStore>>, url: String) -> Res<video::Playlist> {
    let url = url.trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(i18n::t("Enter a playlist address (https://…)."));
    }
    let exe = video::locate_ytdlp(&settings.get().ytdlp_path).ok_or_else(|| i18n::t("yt-dlp not found. Use “Install yt-dlp” in Settings ▸ Video."))?;
    match video::list_playlist(&exe, &url, true).await {
        Ok(p) => Ok(p),
        Err(first) => video::list_playlist(&exe, &url, false).await.map_err(|_| first),
    }
}

#[derive(serde::Deserialize)]
pub struct PlaylistPick {
    url: String,
    title: String,
    index: usize,
}

/// Queues the picked videos into Video\<playlist> (Music\<playlist> for audio). `quality`: "settings", "best",
/// a height ("1080"), "m4a" or "mp3".
#[tauri::command]
pub async fn add_playlist(
    settings: State<'_, Arc<SettingsStore>>,
    store: State<'_, Arc<Store>>,
    title: String,
    entries: Vec<PlaylistPick>,
    quality: String,
    numbered: bool,
) -> Res<usize> {
    let container = settings.get().video_container;
    let audio = matches!(quality.as_str(), "m4a" | "mp3").then(|| quality.clone());
    let format = if audio.is_some() { None } else { video::format_for_quality(&quality, &container) };
    let width = entries.iter().map(|e| e.index).max().unwrap_or(1).to_string().len().max(2);
    let folder = crate::store::folder_name(&title);
    let mut added = 0;
    for e in entries {
        let name: String = e.title.trim().chars().take(110).collect();
        let name = if numbered { format!("{:0width$} - {name}", e.index) } else { name };
        let req = NewDownload {
            url: e.url,
            kind: DownloadKind::Video,
            format: format.clone(),
            audio_format: audio.clone(),
            title: Some(name),
            subfolder: Some(folder.clone()),
            quiet: true,
            ..Default::default()
        };
        if store.add(req).is_some() {
            added += 1;
        }
    }
    Ok(added)
}

#[tauri::command]
pub async fn download_action(store: State<'_, Arc<Store>>, ids: Vec<Uuid>, action: String) -> Res<()> {
    for id in ids {
        match action.as_str() {
            "pause" => store.pause(id),
            "resume" => store.resume(id),
            "cancel" => store.cancel(id),
            "remove" => store.remove(id, false),
            "trash" => store.remove(id, true),
            "open" => store.open(id),
            "reveal" => store.reveal(id),
            _ => return Err(format!("unknown action {action}")),
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn queue_action(store: State<'_, Arc<Store>>, action: String) -> Res<()> {
    match action.as_str() {
        "pauseAll" => store.pause_all(),
        "resumeAll" => store.resume_all(),
        "clearFinished" => store.clear_finished(),
        _ => return Err(format!("unknown action {action}")),
    }
    Ok(())
}

#[tauri::command]
pub async fn copy_url(store: State<'_, Arc<Store>>, id: Uuid) -> Res<()> {
    if let Some(url) = store.url_of(id) {
        clipboard::write_text(&url);
    }
    Ok(())
}

#[tauri::command]
pub async fn copy_text(text: String) -> Res<()> {
    clipboard::write_text(&text);
    Ok(())
}

#[tauri::command]
pub async fn read_clipboard() -> Res<String> {
    Ok(tokio::task::spawn_blocking(clipboard::read_text).await.ok().flatten().unwrap_or_default())
}

// MARK: - Windows

#[tauri::command]
pub async fn show_detail(app: AppHandle, store: State<'_, Arc<Store>>, id: Uuid) -> Res<()> {
    let completed = store.item_view(id).is_some_and(|i| i.state == crate::models::DownloadState::Completed);
    windows::show_detail(&app, id, completed);
    Ok(())
}

#[tauri::command]
pub async fn show_main(app: AppHandle, action: Option<String>) -> Res<()> {
    windows::show_main(&app, action.filter(|a| a == "add" || a == "playlist"));
    Ok(())
}

#[tauri::command]
pub async fn show_settings(app: AppHandle, tab: Option<String>) -> Res<()> {
    windows::show_settings(&app, tab);
    Ok(())
}

#[tauri::command]
pub async fn show_wizard(app: AppHandle) -> Res<()> {
    windows::show_wizard(&app);
    Ok(())
}

/// Resizes the calling window (detail window modes, prompt fitted to its content).
#[tauri::command]
pub async fn window_fit(window: WebviewWindow, width: f64, height: f64, show: Option<bool>) -> Res<()> {
    window.set_size(LogicalSize::new(width, height)).map_err(|e| e.to_string())?;
    if show == Some(true) {
        let _ = window.center();
        let _ = window.show();
        let _ = window.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub async fn window_set_title(window: WebviewWindow, title: String) -> Res<()> {
    window.set_title(&title).map_err(|e| e.to_string())
}

// MARK: - Settings

#[tauri::command]
pub async fn get_settings(settings: State<'_, Arc<SettingsStore>>) -> Res<Settings> {
    Ok(settings.get())
}

#[tauri::command]
pub async fn set_settings(
    app: AppHandle,
    settings: State<'_, Arc<SettingsStore>>,
    server: State<'_, Arc<Server>>,
    patch: Value,
) -> Res<Settings> {
    let old_port = settings.get().server_port;
    let new = settings.patch(&app, patch)?;
    if new.server_port != old_port {
        server.start();
    }
    Ok(new)
}

/// The yt-dlp format arguments the current video settings produce (shown in Settings ▸ Video).
#[tauri::command]
pub async fn effective_ytdlp_args(settings: State<'_, Arc<SettingsStore>>) -> Res<String> {
    Ok(settings.get().ytdlp_format_arguments().join(" "))
}

#[tauri::command]
pub async fn finish_onboarding(app: AppHandle, settings: State<'_, Arc<SettingsStore>>, store: State<'_, Arc<Store>>) -> Res<()> {
    settings.update(&app, |s| s.onboarding_done = true);
    store.prepare_download_folder();
    Ok(())
}

/// Turns "start with Windows" on at the first run of an installed copy, so the app is waiting in the tray after
/// every boot and browser extensions always find the bridge. Only once (the user can switch it off in Settings),
/// and only for an installed copy; a development build would leave a stale entry behind.
pub fn register_autostart_by_default(app: &AppHandle) {
    let settings = app.state::<Arc<SettingsStore>>();
    if settings.get().autostart_auto_registered {
        return;
    }
    let installed = std::env::current_exe().is_ok_and(|p| !p.to_string_lossy().to_lowercase().contains(r"\target\"));
    if !installed {
        return;
    }
    settings.update(app, |s| s.autostart_auto_registered = true);
    let _ = app.autolaunch().enable();
}

#[tauri::command]
pub async fn get_autostart(app: AppHandle) -> Res<bool> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_autostart(app: AppHandle, enabled: bool) -> Res<bool> {
    let al = app.autolaunch();
    if enabled { al.enable() } else { al.disable() }.map_err(|e| e.to_string())?;
    al.is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn open_folder(settings: State<'_, Arc<SettingsStore>>, which: String) -> Res<()> {
    let path = match which.as_str() {
        "downloads" => settings.get().download_dir(),
        "plugins" => browsers::plugins_root(),
        other => Browser::from_key(other.trim_start_matches("browser:")).map(|b| b.plugin_folder()).ok_or("unknown folder")?,
    };
    util::reveal_in_explorer(&path);
    Ok(())
}

// MARK: - Bridge & browsers

#[tauri::command]
pub async fn server_status(server: State<'_, Arc<Server>>) -> Res<ServerStatus> {
    Ok(server.status())
}

#[tauri::command]
pub async fn browsers_status() -> Res<Vec<BrowserInfo>> {
    tokio::task::spawn_blocking(browsers::list).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn prepare_browser_files(key: String) -> Res<String> {
    let b = Browser::from_key(&key).ok_or("unknown browser")?;
    tokio::task::spawn_blocking(move || browsers::prepare_files(b).map(|p| p.to_string_lossy().into_owned()))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn install_signed_addon(key: String) -> Res<()> {
    browsers::install_signed_addon(Browser::from_key(&key).ok_or("unknown browser")?)
}

/// Returns true when the address has to be pasted by hand (Edge does not open edge:// pages for other apps);
/// it is on the clipboard either way.
#[tauri::command]
pub async fn open_extensions_page(key: String) -> Res<bool> {
    let b = Browser::from_key(&key).ok_or("unknown browser")?;
    clipboard::write_text(b.extensions_page());
    browsers::open_extensions_page(b)
}

// MARK: - Tools, site login, updates

#[tauri::command]
pub async fn tools_status(tools: State<'_, Arc<Tools>>) -> Res<ToolsStatus> {
    Ok(tools.status())
}

#[tauri::command]
pub async fn tools_refresh(tools: State<'_, Arc<Tools>>, settings: State<'_, Arc<SettingsStore>>) -> Res<ToolsStatus> {
    tools.refresh(settings.get().ytdlp_path).await;
    Ok(tools.status())
}

#[tauri::command]
pub async fn install_tool(tools: State<'_, Arc<Tools>>, settings: State<'_, Arc<SettingsStore>>, tool: String) -> Res<String> {
    let tools = tools.inner().clone();
    let result = match tool.as_str() {
        "ytdlp" => tools.install_ytdlp().await,
        "deno" => tools.install_deno().await,
        _ => Err("unknown tool".into()),
    };
    tools.refresh(settings.get().ytdlp_path).await;
    result
}

#[tauri::command]
pub async fn login_status(app: AppHandle) -> Res<LoginStatus> {
    Ok(sitelogin::status(&app))
}

#[tauri::command]
pub async fn login_open(app: AppHandle) -> Res<()> {
    sitelogin::open(app).await;
    Ok(())
}

#[tauri::command]
pub async fn login_save(app: AppHandle) -> Res<usize> {
    sitelogin::save(&app).await
}

#[tauri::command]
pub async fn login_clear(app: AppHandle) -> Res<()> {
    sitelogin::clear(&app);
    Ok(())
}

#[tauri::command]
pub async fn updater_status(updater: State<'_, Arc<Updater>>) -> Res<UpdaterStatus> {
    Ok(updater.status())
}

#[tauri::command]
pub async fn check_updates(updater: State<'_, Arc<Updater>>) -> Res<()> {
    let updater = updater.inner().clone();
    tauri::async_runtime::spawn(async move { updater.check(true).await });
    Ok(())
}

// MARK: - Prompt windows

#[tauri::command]
pub async fn prompt_get(id: u64) -> Res<Option<PromptSpec>> {
    Ok(prompt::spec(id))
}

#[tauri::command]
pub async fn prompt_answer(window: WebviewWindow, id: u64, button: Option<usize>, checked: bool) -> Res<()> {
    prompt::reply(id, button, checked);
    let _ = window.close();
    Ok(())
}
