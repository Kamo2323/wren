//! Wren — local dictation for Windows. See docs/design/SPEC.md.

pub mod commands;
pub mod constants;
pub mod dictionary;
pub mod logging;
pub mod paths;
pub mod popup;
pub mod settings;
pub mod tray;

use parking_lot::RwLock;
use paths::Paths;
use settings::Settings;
use tauri::Manager;

/// State shared with commands. Later tasks add fields (store, session handle…).
pub struct AppState {
    pub paths: Paths,
    pub settings: RwLock<Settings>,
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    let start_hidden = args.iter().any(|a| a == "--hidden");

    let paths = Paths::resolve();
    if let Err(err) = paths.ensure_dirs() {
        eprintln!("Wren: cannot create data folders: {err}");
    }
    let _log_guard = logging::init(&paths.logs_dir());
    let settings = Settings::load(&paths.settings_file());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), start_hidden, "Wren starting");

    tauri::Builder::default()
        // Must be first: a second launch just brings the running Wren forward.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            tray::show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .manage(AppState {
            paths,
            settings: RwLock::new(settings),
        })
        .setup(move |app| {
            tray::build(app.handle())?;
            popup::create(app.handle())?;
            if !start_hidden {
                if let Some(main) = app.get_webview_window(tray::MAIN_LABEL) {
                    main.show()?;
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window hides it to the tray; Quit lives in the tray menu.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == tray::MAIN_LABEL {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_constants,
            commands::app_version,
            commands::get_settings,
            commands::save_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wren");
}
