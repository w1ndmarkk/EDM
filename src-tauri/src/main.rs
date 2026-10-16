// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod browsers;
mod clipboard;
mod commands;
mod i18n;
mod models;
mod paths;
mod prompt;
mod segmented;
mod server;
mod settings;
mod sitelogin;
mod store;
mod tools;
mod tray;
mod updater;
mod util;
mod video;
mod windows;

use std::sync::Arc;

use tauri::{Manager, RunEvent};

use crate::server::Server;
use crate::settings::SettingsStore;
use crate::store::Store;
use crate::tools::Tools;
use crate::updater::Updater;

/// Command-line flag of the "start with Windows" registry entry.
const AUTOSTART_ARG: &str = "--autostart";

fn main() {
    let app = tauri::Builder::default()
        // A second launch from the Start menu brings the running instance forward (a boot launch stays quiet).
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().any(|a| a == AUTOSTART_ARG) {
                windows::show_main(app, None);
            }
        }))
        .plugin(tauri_plugin_autostart::Builder::new().args([AUTOSTART_ARG]).build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            if let Ok(dir) = app.path().resource_dir() {
                paths::set_resource_dir(dir);
            }
            let handle = app.handle().clone();
            let settings = Arc::new(SettingsStore::load());
            let store = Store::new(handle.clone(), settings.clone());
            let tools = Arc::new(Tools::new(handle.clone()));
            let server = Server::new(handle.clone(), store.clone(), settings.clone());
            let updater = Updater::new(handle.clone(), store.clone(), settings.clone());
            app.manage(settings.clone());
            app.manage(store.clone());
            app.manage(tools.clone());
            app.manage(server.clone());
            app.manage(updater.clone());

            tray::create(&handle)?;
            server.start();
            store.start_background();
            updater.start();
            let custom = settings.get().ytdlp_path;
            tauri::async_runtime::spawn(async move { tools.refresh(custom).await });

            let s = settings.get();
            if s.onboarding_done {
                store.prepare_download_folder();
            }
            commands::register_autostart_by_default(&handle);
            // Started with Windows: stay in the tray. Started by hand: open the window (unless the user chose otherwise).
            let at_boot = std::env::args().any(|a| a == AUTOSTART_ARG);
            if !at_boot && !(s.start_in_tray && s.onboarding_done) {
                windows::show_main(&handle, None);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::bootstrap,
            commands::get_item,
            commands::get_item_log,
            commands::add_urls,
            commands::fetch_playlist,
            commands::add_playlist,
            commands::download_action,
            commands::queue_action,
            commands::copy_url,
            commands::copy_text,
            commands::read_clipboard,
            commands::show_detail,
            commands::show_main,
            commands::show_settings,
            commands::show_wizard,
            commands::window_fit,
            commands::window_set_title,
            commands::get_settings,
            commands::set_settings,
            commands::effective_ytdlp_args,
            commands::finish_onboarding,
            commands::get_autostart,
            commands::set_autostart,
            commands::open_folder,
            commands::server_status,
            commands::browsers_status,
            commands::prepare_browser_files,
            commands::open_extensions_page,
            commands::tools_status,
            commands::tools_refresh,
            commands::install_tool,
            commands::login_status,
            commands::login_open,
            commands::login_save,
            commands::login_clear,
            commands::updater_status,
            commands::check_updates,
            commands::prompt_get,
            commands::prompt_answer,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the application");

    app.run(|app, event| match event {
        // Closing the last window keeps the app in the tray; only "Quit" (exit code set) ends it.
        RunEvent::ExitRequested { api, code, .. } if code.is_none() => api.prevent_exit(),
        RunEvent::Exit => {
            if let Some(store) = app.try_state::<Arc<Store>>() {
                store.shutdown();
            }
        }
        _ => {}
    });
}
