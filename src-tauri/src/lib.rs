//! Wren — local dictation for Windows. See docs/design/SPEC.md.

pub mod commands;
pub mod constants;
pub mod dictionary;
pub mod logging;
pub mod paths;
pub mod popup;
pub mod settings;
pub mod store;
pub mod tray;

use parking_lot::RwLock;
use paths::Paths;
use settings::Settings;
use store::Store;
use tauri::Manager;

/// State shared with commands. Later tasks add fields (session handle…).
pub struct AppState {
    pub paths: Paths,
    pub settings: RwLock<Settings>,
    /// History, dictionary and stats. If the database could not be opened this is an
    /// unavailable store whose calls all return the reason; the app still runs.
    pub store: Store,
}

/// Open the database, or log why not and carry on with an unavailable store.
fn open_store(paths: &Paths) -> Store {
    match Store::open(&paths.db_file()) {
        Ok(store) => store,
        Err(err) => {
            tracing::error!(error = %err, "cannot open the database; History and the dictionary are unavailable");
            Store::unavailable(err.to_string())
        }
    }
}

/// One retention sweep (spec §9). Logs counts only, never text.
fn sweep_store(store: &Store, paths: &Paths) {
    if store.unavailable_reason().is_some() {
        return;
    }
    match store.sweep(chrono::Utc::now(), &paths.audio_dir()) {
        Ok(r) => tracing::info!(
            rows_deleted = r.rows_deleted,
            audio_deleted = r.audio_deleted,
            orphan_audio_deleted = r.orphan_audio_deleted,
            events_deleted = r.events_deleted,
            "retention sweep"
        ),
        Err(err) => tracing::warn!(error = %err, "retention sweep failed"),
    }
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
        .setup(move |app| {
            // The database is opened here, after the plugins' setup, so a second launch (which
            // the single-instance plugin ends before this point) never opens, migrates or sweeps it.
            let store = open_store(&paths);
            app.manage(AppState {
                paths,
                settings: RwLock::new(settings),
                store,
            });
            tray::build(app.handle())?;
            popup::create(app.handle())?;
            // Retention sweep now (off the main thread) and then every SWEEP_INTERVAL_MS.
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("wren-sweep".into())
                .spawn(move || loop {
                    let state = handle.state::<AppState>();
                    sweep_store(&state.store, &state.paths);
                    std::thread::sleep(std::time::Duration::from_millis(constants::SWEEP_INTERVAL_MS));
                })?;
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
            commands::history_recent,
            commands::history_search,
            commands::history_delete,
            commands::history_delete_all,
            commands::history_fix_word,
            commands::stats_get,
            commands::dictionary_list,
            commands::dictionary_add_term,
            commands::dictionary_add_replacement,
            commands::dictionary_set_status,
            commands::dictionary_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wren");
}
