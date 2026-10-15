//! Notification-area icon: the app keeps running here so browser extensions can hand over downloads at any time.

use std::sync::{Arc, Mutex};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::i18n::t;
use crate::server::Server;
use crate::store::{Store, Summary};
use crate::updater::Updater;
use crate::{tf, util, windows};

pub struct TrayState {
    tray: TrayIcon,
    status: MenuItem<tauri::Wry>,
    bridge: MenuItem<tauri::Wry>,
    idle_icon: Image<'static>,
    active_icon: Image<'static>,
    last: Mutex<Option<(Summary, bool)>>,
}

/// The app icon with a green "downloading" dot in the lower right corner.
fn with_badge(icon: &Image<'_>) -> Image<'static> {
    let (w, h) = (icon.width() as i32, icon.height() as i32);
    let mut rgba = icon.rgba().to_vec();
    let r = (w.min(h) as f32 * 0.22).max(3.0);
    let (cx, cy) = (w as f32 - r - 1.0, h as f32 - r - 1.0);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let i = ((y * w + x) * 4) as usize;
            if d <= r {
                rgba[i..i + 4].copy_from_slice(&[46, 189, 89, 255]);
            } else if d <= r + 1.2 {
                rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Image::new_owned(rgba, w as u32, h as u32)
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", t("No active downloads"), false, None::<&str>)?;
    let bridge = MenuItem::with_id(app, "bridge", t("Bridge is not running"), false, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &bridge,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "show", t("Show Window"), true, None::<&str>)?,
            &MenuItem::with_id(app, "new", t("New Download…"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "pause_all", t("Pause All"), true, None::<&str>)?,
            &MenuItem::with_id(app, "resume_all", t("Resume All"), true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, "settings", t("Settings…"), true, None::<&str>)?,
        ],
    )?;
    if crate::updater::REPO.is_some() {
        menu.append(&MenuItem::with_id(app, "updates", t("Check for Updates…"), true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", t("Quit EDM"), true, None::<&str>)?)?;
    let base = app.default_window_icon().cloned().ok_or(tauri::Error::InvalidIcon(std::io::Error::other("no icon")))?;
    let idle_icon = Image::new_owned(base.rgba().to_vec(), base.width(), base.height());
    let active_icon = with_badge(&base);

    let tray = TrayIconBuilder::with_id("main")
        .icon(idle_icon.clone())
        .tooltip("EDM")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let app = app.clone();
            match event.id().as_ref() {
                "show" => windows::show_main(&app, None),
                "new" => windows::show_main(&app, Some("add".into())),
                "settings" => windows::show_settings(&app, None),
                "pause_all" => {
                    let store = app.state::<Arc<Store>>().inner().clone();
                    tauri::async_runtime::spawn(async move { store.pause_all() });
                }
                "resume_all" => {
                    let store = app.state::<Arc<Store>>().inner().clone();
                    tauri::async_runtime::spawn(async move { store.resume_all() });
                }
                "updates" => {
                    let updater = app.state::<Arc<Updater>>().inner().clone();
                    tauri::async_runtime::spawn(async move { updater.check(true).await });
                }
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                windows::show_main(tray.app_handle(), None);
            }
        })
        .build(app)?;
    app.manage(TrayState { tray, status, bridge, idle_icon, active_icon, last: Mutex::new(None) });
    Ok(())
}

/// Refreshes the status lines, tooltip and icon (called from the store tick; cheap when nothing changed).
pub fn update(app: &AppHandle, summary: &Summary) {
    let Some(st) = app.try_state::<TrayState>() else { return };
    let running = app.try_state::<Arc<Server>>().is_some_and(|s| s.status().running);
    // Round the speed so the menu is not rewritten for every byte.
    let mut key = summary.clone();
    key.total_speed = (key.total_speed / 10_000.0).round();
    key.overall_progress = key.overall_progress.map(|p| (p * 100.0).floor());
    {
        let mut last = st.last.lock().unwrap();
        if last.as_ref() == Some(&(key.clone(), running)) {
            return;
        }
        let was_active = last.as_ref().is_some_and(|(s, _)| s.active_count > 0);
        let now_active = summary.active_count > 0;
        if last.is_none() || was_active != now_active {
            let _ = st.tray.set_icon(Some(if now_active { st.active_icon.clone() } else { st.idle_icon.clone() }));
        }
        *last = Some((key, running));
    }
    let speed = tf!("{0}/s", util::fmt_size(summary.total_speed as i64));
    if summary.active_count > 0 {
        let _ = st.status.set_text(tf!("{0} downloading · {1}", summary.active_count, speed));
        let pct = summary.overall_progress.map(|p| format!("{}% · ", (p * 100.0) as i64)).unwrap_or_default();
        let _ = st.tray.set_tooltip(Some(format!("EDM — {pct}{speed}")));
    } else {
        let _ = st.status.set_text(t("No active downloads"));
        let _ = st.tray.set_tooltip(Some("EDM"));
    }
    let _ = st.bridge.set_text(if running { t("Bridge is running") } else { t("Bridge is not running") });
}
