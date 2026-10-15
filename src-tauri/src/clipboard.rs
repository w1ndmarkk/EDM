//! Watches the clipboard for http(s) links and asks whether to download them.
//! Also handles Ctrl+V in the main window.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Url;
use tauri::AppHandle;

use crate::i18n::t;
use crate::models::{DownloadKind, FileCategory};
use crate::prompt::{self, PromptSpec};
use crate::settings::SettingsStore;
use crate::store::{NewDownload, Store};
use crate::tf;
use crate::video::host_matches;

const VIDEO_HOSTS: &[&str] = &[
    "youtube.com", "youtu.be", "vimeo.com", "dailymotion.com", "twitter.com", "x.com", "instagram.com", "tiktok.com", "twitch.tv",
    "facebook.com", "reddit.com", "bilibili.com",
];

#[derive(PartialEq, Clone, Copy)]
enum Source {
    Clipboard,
    Paste,
}

#[derive(Default)]
struct State {
    asked: Vec<String>,
    prompting: bool,
}

pub struct ClipboardWatcher {
    app: AppHandle,
    store: Arc<Store>,
    settings: Arc<SettingsStore>,
    state: Mutex<State>,
}

fn sequence_number() -> u32 {
    unsafe { windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

pub fn read_text() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok()
}

impl ClipboardWatcher {
    pub fn new(app: AppHandle, store: Arc<Store>, settings: Arc<SettingsStore>) -> Arc<Self> {
        Arc::new(Self { app, store, settings, state: Mutex::new(State::default()) })
    }

    pub fn start(self: &Arc<Self>) {
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            let mut last = sequence_number();
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;
                if !me.settings.get().watch_clipboard || me.state.lock().unwrap().prompting {
                    continue;
                }
                // The sequence number changes on every copy; the text is only read when it did.
                let seq = sequence_number();
                if seq == last {
                    continue;
                }
                last = seq;
                let Ok(Some(text)) = tokio::task::spawn_blocking(read_text).await else { continue };
                me.check(&text).await;
            }
        });
    }

    async fn check(self: &Arc<Self>, text: &str) {
        // Only react when the clipboard holds nothing but links, and the links point at files of a captured type.
        let Some(urls) = url_only_content(text) else { return };
        let settings = self.settings.get();
        let urls: Vec<Url> = {
            let st = self.state.lock().unwrap();
            urls.into_iter().filter(|u| settings.should_capture(u, None) && !st.asked.contains(&u.to_string())).collect()
        };
        if urls.is_empty() {
            return;
        }
        self.remember(&urls);
        self.ask(urls, Source::Clipboard).await;
    }

    /// Ctrl+V in the main window (explicit user action: any http(s) link is accepted).
    pub async fn handle_paste(self: &Arc<Self>, text: &str) {
        let Some(urls) = url_only_content(text) else { return };
        self.remember(&urls);
        self.ask(urls, Source::Paste).await;
    }

    /// Copies text for the user; links copied by the app itself never trigger the prompt.
    pub fn copy_text(&self, text: &str) {
        if let Some(urls) = url_only_content(text) {
            self.remember(&urls);
        }
        if let Ok(mut cb) = arboard::Clipboard::new() {
            let _ = cb.set_text(text.to_string());
        }
    }

    fn remember(&self, urls: &[Url]) {
        let mut st = self.state.lock().unwrap();
        st.asked.extend(urls.iter().map(Url::to_string));
        let n = st.asked.len();
        if n > 50 {
            st.asked.drain(..n - 50);
        }
    }

    async fn ask(self: &Arc<Self>, urls: Vec<Url>, source: Source) {
        {
            let mut st = self.state.lock().unwrap();
            if st.prompting {
                return;
            }
            st.prompting = true;
        }
        let (title, detail) = if urls.len() == 1 {
            let title = if source == Source::Clipboard { t("A link was copied. Download it?") } else { t("Download the pasted link?") };
            (title, urls[0].to_string())
        } else {
            let title = if source == Source::Clipboard {
                tf!("{0} links were copied. Download them?", urls.len())
            } else {
                tf!("Download the {0} pasted links?", urls.len())
            };
            let mut list: Vec<String> = urls.iter().take(6).map(Url::to_string).collect();
            if urls.len() > 6 {
                list.push("…".into());
            }
            (title, list.join("\n"))
        };
        let prefer_video = urls.iter().all(looks_like_video_page);
        let buttons = if prefer_video {
            vec![t("Download Video (yt-dlp)"), t("Download as File"), t("Ignore")]
        } else {
            vec![t("Download"), t("Download Video (yt-dlp)"), t("Ignore")]
        };
        let spec = PromptSpec {
            title,
            detail: Some(detail),
            buttons,
            default_button: 0,
            cancel_button: Some(2),
            checkbox: (source == Source::Clipboard).then(|| t("Stop watching the clipboard")),
            ..Default::default()
        };
        let answer = prompt::ask(&self.app, spec).await;
        self.state.lock().unwrap().prompting = false;
        if answer.checked {
            self.settings.update(&self.app, |s| s.watch_clipboard = false);
        }
        let kind = match answer.button {
            Some(0) if prefer_video => DownloadKind::Video,
            Some(0) => DownloadKind::File,
            Some(1) if prefer_video => DownloadKind::File,
            Some(1) => DownloadKind::Video,
            _ => return,
        };
        for url in urls {
            self.store.add(NewDownload { url: url.to_string(), kind, ..Default::default() });
        }
    }
}

/// The links, if every non-empty line of `text` is a single http(s) URL; None otherwise.
pub fn url_only_content(text: &str) -> Option<Vec<Url>> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() || lines.len() > 50 {
        return None;
    }
    let mut urls: Vec<Url> = vec![];
    for line in lines {
        if line.contains(' ') {
            return None;
        }
        let url = Url::parse(line).ok().filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())?;
        if !urls.contains(&url) {
            urls.push(url);
        }
    }
    Some(urls)
}

fn looks_like_video_page(url: &Url) -> bool {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    if FileCategory::detect(last, DownloadKind::File, false) != FileCategory::Other {
        return false;
    }
    url.host_str().is_some_and(|h| host_matches(h, VIDEO_HOSTS))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_only_text() {
        assert_eq!(url_only_content("https://a.com/x.zip\n\nhttp://b.org/y").map(|v| v.len()), Some(2));
        assert!(url_only_content("see https://a.com/x.zip").is_none());
        assert!(url_only_content("ftp://a.com/x").is_none());
        assert!(looks_like_video_page(&Url::parse("https://www.youtube.com/watch?v=1").unwrap()));
        assert!(!looks_like_video_page(&Url::parse("https://youtube.com/file.zip").unwrap()));
    }
}
