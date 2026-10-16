//! Browser extension helpers. Supported browsers for the guided setup: Google Chrome, Microsoft Edge, Brave and
//! Mozilla Firefox. Chromium browsers only install extensions silently from their stores, so the extension is
//! loaded unpacked from a folder the app prepares; Firefox loads it as a temporary add-on (until it is signed).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use crate::paths;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Browser {
    Chrome,
    Edge,
    Brave,
    Firefox,
}

impl Browser {
    pub const ALL: [Browser; 4] = [Browser::Chrome, Browser::Edge, Browser::Brave, Browser::Firefox];

    pub fn key(self) -> &'static str {
        match self {
            Self::Chrome => "chrome",
            Self::Edge => "edge",
            Self::Brave => "brave",
            Self::Firefox => "firefox",
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
            Self::Firefox => "Mozilla Firefox",
        }
    }

    fn exe_name(self) -> &'static str {
        match self {
            Self::Chrome => "chrome.exe",
            Self::Edge => "msedge.exe",
            Self::Brave => "brave.exe",
            Self::Firefox => "firefox.exe",
        }
    }

    pub fn extensions_page(self) -> &'static str {
        match self {
            Self::Chrome => "chrome://extensions",
            Self::Edge => "edge://extensions",
            Self::Brave => "brave://extensions",
            Self::Firefox => "about:debugging#/runtime/this-firefox",
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
            Self::Firefox => r"Mozilla Firefox\firefox.exe",
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
    let steps = if b == Browser::Firefox {
        firefox_flavor_file(&dest)?;
        "1. Open about:debugging#/runtime/this-firefox in Firefox.\r\n\
         2. Click \"Load Temporary Add-on...\" and select manifest.json in this folder.\r\n\
         Firefox removes temporary add-ons when it closes; load it again after a restart\r\n\
         (a signed EDM add-on stays installed).\r\n"
            .to_string()
    } else {
        format!(
            "1. Open {page} in {name}.\r\n\
             2. Turn on \"Developer mode\".\r\n\
             3. Click \"Load unpacked\" and select this folder.\r\n",
            name = b.name(),
            page = b.extensions_page()
        )
    };
    let readme = format!(
        "EDM — {name} extension\r\n\r\n{steps}\r\nDo not move or delete this folder while the extension is installed.\r\n",
        name = b.name()
    );
    let _ = std::fs::write(dest.join("README.txt"), readme);
    Ok(dest)
}

/// Add-on ID on addons.mozilla.org; it must not change once the add-on is signed. ("edm@w1ndmarkk" belonged to a
/// deleted submission: AMO never reuses the ID of a deleted add-on.)
pub const FIREFOX_ADDON_ID: &str = "edm-extension@w1ndmarkk";

/// Turns the Chrome manifest into Firefox's (keep scripts/pack-extension.ps1 in step):
/// a background script instead of a service worker, the add-on ID, no Chrome-only keys, and blocking webRequest
/// (Firefox still allows it: downloads are stopped at the response headers, before any of the file arrives).
pub fn firefox_flavor(m: &mut Value) {
    m["background"] = json!({ "scripts": ["hls.js", "rules.js", "background.js"] });
    if let Some(perms) = m["permissions"].as_array_mut() {
        if !perms.iter().any(|p| p == "webRequestBlocking") {
            perms.push(json!("webRequestBlocking"));
        }
    }
    m["browser_specific_settings"] = json!({
        "gecko": {
            "id": FIREFOX_ADDON_ID,
            // data_collection_permissions: Firefox 140, Firefox for Android 142 (content_scripts "world": "MAIN" needs 128)
            "strict_min_version": "142.0",
            "data_collection_permissions": { "required": ["none"] }
        }
    });
    if let Some(o) = m.as_object_mut() {
        o.remove("minimum_chrome_version");
    }
    if let Some(scripts) = m["content_scripts"].as_array_mut() {
        for cs in scripts.iter_mut().filter_map(Value::as_object_mut) {
            cs.remove("match_origin_as_fallback");
            cs.insert("match_about_blank".into(), Value::Bool(true));
        }
    }
}

fn firefox_flavor_file(dir: &Path) -> Result<(), String> {
    let path = dir.join("manifest.json");
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut m: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    firefox_flavor(&mut m);
    std::fs::write(&path, serde_json::to_string_pretty(&m).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Opens the browser's extensions page (chrome:// URLs cannot be opened through the shell, only as an argument).
/// Edge ignores edge:// addresses from the command line and shows a new tab instead: it gets a new window, and the
/// user pastes the address. Returns true in that case. Firefox opens about:debugging from the command line.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_manifest_from_the_chrome_one() {
        let mut m: Value = serde_json::from_str(include_str!("../../Extension/manifest.json")).unwrap();
        firefox_flavor(&mut m);
        assert_eq!(m["background"], json!({ "scripts": ["hls.js", "rules.js", "background.js"] }));
        assert!(m["permissions"].as_array().unwrap().iter().any(|p| p == "webRequestBlocking"));
        assert_eq!(m["browser_specific_settings"]["gecko"]["id"], FIREFOX_ADDON_ID);
        assert!(m.get("minimum_chrome_version").is_none());
        for cs in m["content_scripts"].as_array().unwrap() {
            assert!(cs.get("match_origin_as_fallback").is_none());
            assert_eq!(cs["match_about_blank"], true);
        }
        // Everything else stays as it is.
        assert_eq!(m["manifest_version"], 3);
        assert_eq!(m["content_scripts"][0]["world"], "MAIN");
    }
}
