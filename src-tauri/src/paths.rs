use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static RESOURCE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Called once at startup with Tauri's resource directory (bundled tools and the browser extension live there).
pub fn set_resource_dir(dir: PathBuf) {
    let _ = RESOURCE_DIR.set(strip_verbatim(dir));
}

/// `\\?\C:\…` → `C:\…` (the resource path arrives canonicalized; child processes and the UI want the plain form).
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

pub fn resource_dir() -> PathBuf {
    RESOURCE_DIR.get().cloned().unwrap_or_else(|| {
        std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_default()
    })
}

/// %LOCALAPPDATA%\com.edm.downloadmanager — settings, download list, cookies, user-installed tools.
/// Not %LOCALAPPDATA%\EDM: that is where the per-user installer puts the program itself.
/// The uninstaller's "delete app data" option removes this folder. `EDM_DATA_DIR` points it elsewhere (development, tests).
pub fn data_dir() -> PathBuf {
    let dir = match std::env::var_os("EDM_DATA_DIR") {
        Some(d) => PathBuf::from(d),
        None => dirs::data_local_dir().unwrap_or_else(std::env::temp_dir).join("com.edm.downloadmanager"),
    };
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn bin_dir() -> PathBuf {
    data_dir().join("bin")
}

pub fn cookies_dir() -> PathBuf {
    data_dir().join("cookies")
}

/// Stream playlists the page built itself (blob: URLs): saved here for yt-dlp.
pub fn manifests_dir() -> PathBuf {
    data_dir().join("manifests")
}

/// Bundled helper binaries (yt-dlp.exe, ffmpeg.exe).
pub fn bundled_tools_dir() -> PathBuf {
    resource_dir().join("tools")
}

/// Unpacked copy of the browser extension shipped with the app.
pub fn bundled_extension_dir() -> PathBuf {
    if let Ok(p) = std::env::var("EDM_EXTENSION_DIR") {
        return PathBuf::from(p);
    }
    resource_dir().join("extension")
}

/// The user's Downloads folder (like IDM: category subfolders are created inside it, other files land in it directly).
pub fn default_download_dir() -> PathBuf {
    dirs::download_dir().or_else(|| dirs::home_dir().map(|h| h.join("Downloads"))).unwrap_or_else(std::env::temp_dir)
}

/// Finds `name` (e.g. "yt-dlp.exe") in the directories of %PATH%.
pub fn search_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}
