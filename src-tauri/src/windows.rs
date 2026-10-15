//! Window management. On Windows, building a webview window from a synchronous command or the main
//! thread can deadlock (WebView2), so every window is created from an async task.

use std::time::Duration;

use tauri::webview::PageLoadEvent;
use tauri::window::Color;
use tauri::{AppHandle, Emitter, Manager, Runtime, Theme, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use uuid::Uuid;

pub const DETAIL_WIDTH: f64 = 580.0;
pub const DETAIL_HEIGHT: f64 = 560.0;
pub const DETAIL_COMPLETED_HEIGHT: f64 = 290.0;

fn on_ui(app: &AppHandle, f: impl FnOnce(&AppHandle) + Send + 'static) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move { f(&app) });
}

fn focus<R: Runtime>(w: &WebviewWindow<R>) {
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

/// Windows "apps" theme (Settings ▸ Personalization ▸ Colors), which the pages follow via prefers-color-scheme.
pub fn system_dark() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|k| k.get_value::<u32, _>("AppsUseLightTheme"))
        .map(|light| light == 0)
        .unwrap_or(false)
}

/// Paints the window and its webview in the page's background colour (--bg in app.css) instead of WebView2's
/// default white, with a matching title bar.
pub fn painted<'a, R: Runtime, M: Manager<R>>(b: WebviewWindowBuilder<'a, R, M>) -> WebviewWindowBuilder<'a, R, M> {
    let dark = system_dark();
    let bg = if dark { Color(0x20, 0x20, 0x20, 0xff) } else { Color(0xf3, 0xf3, 0xf3, 0xff) };
    b.background_color(bg).theme(Some(if dark { Theme::Dark } else { Theme::Light }))
}

/// No white flash while a window opens: painted in the page's colours, and hidden until its page has loaded.
fn without_flash<'a, R: Runtime, M: Manager<R>>(b: WebviewWindowBuilder<'a, R, M>) -> WebviewWindowBuilder<'a, R, M> {
    painted(b).visible(false).on_page_load(|w, p| {
        if p.event() == PageLoadEvent::Finished && !w.is_visible().unwrap_or(true) {
            focus(&w);
        }
    })
}

/// Shows a window built `without_flash` once its page has loaded, or after a moment if that event never comes.
fn reveal(w: WebviewWindow) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        if !w.is_visible().unwrap_or(true) {
            focus(&w);
        }
    });
}

/// Opens (or brings forward) the main window. `action` is handed to the page: "add" opens the New Download
/// dialog, "playlist" or "playlist:<url>" the playlist dialog.
pub fn show_main(app: &AppHandle, action: Option<String>) {
    on_ui(app, move |app| {
        if let Some(w) = app.get_webview_window("main") {
            focus(&w);
            if let Some(a) = action {
                let _ = w.emit_to("main", "main-action", a);
            }
            return;
        }
        let url = match action {
            Some(a) => {
                let a = percent_encoding::utf8_percent_encode(&a, percent_encoding::NON_ALPHANUMERIC);
                format!("index.html?action={a}")
            }
            None => "index.html".into(),
        };
        let built = WebviewWindowBuilder::new(app, "main", WebviewUrl::App(url.into()))
            .title("EDM")
            .inner_size(1180.0, 680.0)
            .min_inner_size(860.0, 480.0)
            // Links dropped on the window are handled by the page (HTML5 drag and drop).
            .disable_drag_drop_handler()
            .center();
        let built = without_flash(built).build();
        match built {
            Ok(w) => reveal(w),
            Err(e) => eprintln!("main window: {e}"),
        }
    });
}

pub fn show_settings(app: &AppHandle, tab: Option<String>) {
    on_ui(app, move |app| {
        if let Some(w) = app.get_webview_window("settings") {
            focus(&w);
            if let Some(t) = tab {
                let _ = w.emit_to("settings", "settings-tab", t);
            }
            return;
        }
        let url = format!("settings.html#{}", tab.unwrap_or_else(|| "general".into()));
        let built = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App(url.into()))
            .title(crate::i18n::t("Settings"))
            .inner_size(720.0, 640.0)
            .min_inner_size(600.0, 480.0)
            .center();
        let built = without_flash(built).build();
        match built {
            Ok(w) => reveal(w),
            Err(e) => eprintln!("settings window: {e}"),
        }
    });
}

pub fn show_wizard(app: &AppHandle) {
    on_ui(app, |app| {
        if let Some(w) = app.get_webview_window("wizard") {
            focus(&w);
            return;
        }
        let built = WebviewWindowBuilder::new(app, "wizard", WebviewUrl::App("wizard.html".into()))
            .title(crate::i18n::t("Browser Extension Setup"))
            .inner_size(680.0, 520.0)
            .resizable(false)
            .maximizable(false)
            .center();
        let built = without_flash(built).build();
        match built {
            Ok(w) => reveal(w),
            Err(e) => eprintln!("wizard window: {e}"),
        }
    });
}

pub fn detail_label(id: Uuid) -> String {
    format!("detail-{id}")
}

/// Per-download window (IDM style): status, segment bar and connection table.
pub fn show_detail(app: &AppHandle, id: Uuid, completed: bool) {
    on_ui(app, move |app| {
        let label = detail_label(id);
        if let Some(w) = app.get_webview_window(&label) {
            focus(&w);
            return;
        }
        let height = if completed { DETAIL_COMPLETED_HEIGHT } else { DETAIL_HEIGHT };
        let built = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(format!("detail.html?id={id}").into()))
            .title("EDM")
            .inner_size(DETAIL_WIDTH, height)
            .min_inner_size(480.0, 240.0)
            .maximizable(false)
            .center();
        let built = without_flash(built).build();
        match built {
            Ok(w) => reveal(w),
            Err(e) => eprintln!("detail window: {e}"),
        }
    });
}

pub fn close_detail(app: &AppHandle, id: Uuid) {
    if let Some(w) = app.get_webview_window(&detail_label(id)) {
        let _ = w.close();
    }
}
