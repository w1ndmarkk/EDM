//! Browser extension helpers. Supported browsers for the guided setup: Google Chrome, Microsoft Edge, Brave.
//! Chromium browsers only install extensions silently from their stores, so the extension is loaded unpacked
//! from a folder the app prepares.

use std::path::PathBuf;

use serde::Serialize;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use crate::paths;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Browser {
    Chrome,
    Edge,
    Brave,
}

impl Browser {
    pub const ALL: [Browser; 3] = [Browser::Chrome, Browser::Edge, Browser::Brave];

    pub fn key(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Brave => "brave",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.key() == key)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Chrome => "Google Chrome",
            Self::Edge => "Microsoft Edge",
            Self::Brave => "Brave",
        }
    }

    fn exe_name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome.exe",
            Self::Edge => "msedge.exe",
            Self::Brave => "brave.exe",
        }
    }

    pub fn extensions_page(self) -> &'static str {
        match self {
            Self::Chrome => "chrome://extensions",
            Self::Edge => "edge://extensions",
            Self::Brave => "brave://extensions",
        }
    }

    /// Install location from the "App Paths" registration, then the default install folders.
    pub fn exe_path(self) -> Option<PathBuf> {
        let sub = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{}", self.exe_name());
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            if let Ok(key) = RegKey::predef(hive).open_subkey(&sub) {
                if let Ok(p) = key.get_value::<String, _>("") {
                    let p = PathBuf::from(p.trim_matches('"'));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
        let rel = match self {
            Self::Chrome => r"Google\Chrome\Application\chrome.exe",
            Self::Edge => r"Microsoft\Edge\Application\msedge.exe",
            Self::Brave => r"BraveSoftware\Brave-Browser\Application\brave.exe",
        };
        ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(|v| std::env::var_os(v))
            .map(|base| PathBuf::from(base).join(rel))
            .find(|p| p.is_file())
    }

    pub fn plugin_folder(self) -> PathBuf {
        plugins_root().join(self.name())
    }

    pub fn files_prepared(self) -> bool {
        self.plugin_folder().join("manifest.json").is_file()
    }
}

/// %LOCALAPPDATA%\com.edm.downloadmanager\Browser Extension (kept out of the Downloads folder, which now
/// receives the downloads themselves).
pub fn plugins_root() -> PathBuf {
    paths::data_dir().join("Browser Extension")
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct BrowserInfo {
    pub key: &'static str,
    pub name: &'static str,
    pub installed: bool,
    pub files_prepared: bool,
    pub folder: String,
    pub extensions_page: &'static str,
}

pub fn list() -> Vec<BrowserInfo> {
    Browser::ALL
        .into_iter()
        .map(|b| BrowserInfo {
            key: b.key(),
            name: b.name(),
            installed: b.exe_path().is_some(),
            files_prepared: b.files_prepared(),
            folder: b.plugin_folder().to_string_lossy().into_owned(),
            extensions_page: b.extensions_page(),
        })
        .collect()
}

/// Copies the unpacked extension into the browser's plugin folder. Returns the folder.
pub fn prepare_files(b: Browser) -> Result<PathBuf, String> {
    let src = paths::bundled_extension_dir();
    if !src.join("manifest.json").is_file() {
        return Err(format!("{} ({})", crate::i18n::t("The bundled extension files are missing."), src.display()));
    }
    let dest = b.plugin_folder();
    std::fs::create_dir_all(plugins_root()).map_err(|e| e.to_string())?;
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    crate::util::copy_dir(&src, &dest).map_err(|e| e.to_string())?;
    let readme = format!(
        "EDM — {name} extension\r\n\r\n\
         1. Open {page} in {name}.\r\n\
         2. Turn on \"Developer mode\".\r\n\
         3. Click \"Load unpacked\" and select this folder.\r\n\r\n\
         Do not move or delete this folder while the extension is installed.\r\n",
        name = b.name(),
        page = b.extensions_page()
    );
    let _ = std::fs::write(dest.join("README.txt"), readme);
    Ok(dest)
}

/// Opens the browser's extensions page (chrome:// URLs cannot be opened through the shell, only as an argument).
/// Edge ignores edge:// addresses from the command line and shows a new tab instead: it gets a new window, and the
/// user pastes the address. Returns true in that case.
pub fn open_extensions_page(b: Browser) -> Result<bool, String> {
    let exe = b.exe_path().ok_or_else(|| format!("{} not found", b.name()))?;
    let mut cmd = std::process::Command::new(exe);
    let paste = b == Browser::Edge;
    if paste {
        cmd.arg("--new-window");
    } else {
        cmd.arg(b.extensions_page());
    }
    cmd.spawn().map(|_| paste).map_err(|e| e.to_string())
}
