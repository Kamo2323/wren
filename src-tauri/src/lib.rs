//! Wren — local dictation for Windows. See docs/design/SPEC.md.

pub mod audio;
pub mod commands;
pub mod constants;
pub mod dictionary;
pub mod engine;
pub mod hotkey;
pub mod logging;
pub mod paste;
pub mod paths;
pub mod pipeline;
pub mod popup;
pub mod selftest;
pub mod session;
pub mod settings;
pub mod store;
pub mod tray;

use parking_lot::RwLock;
use paths::Paths;
use session::app::Services;
use settings::Settings;
use store::Store;
use tauri::Manager;

/// State shared with commands. The running services (session, engine, hotkey, recorder) are a
/// separate managed `session::app::Services`, started in `setup` once this exists.
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
    let popup_check = args.iter().any(|a| a == "--popup-check");
    let selftest = args.iter().any(|a| a == "--selftest");
    // `wren --quit`: asks a running Wren to quit as if from the tray (installer, live test).
    let quit = args.iter().any(|a| a == "--quit");

    let paths = Paths::resolve();
    if let Err(err) = paths.ensure_dirs() {
        eprintln!("Wren: cannot create data folders: {err}");
    }
    let _log_guard = logging::init(&paths.logs_dir());
    logging::install_panic_hook();
    let settings = Settings::load(&paths.settings_file());
    tracing::info!(version = env!("CARGO_PKG_VERSION"), start_hidden, selftest, quit, "Wren starting");
    if let Some(root) = paths::runtime_root_override() {
        tracing::warn!(
            root = %root.display(),
            "WREN_RUNTIME_ROOT is set: the speech runtime and models are read from this folder (tests only)"
        );
    }
    if selftest {
        // Headless: no window, no single-instance hand-off, no keys sent.
        let code = selftest::run(&paths, &settings);
        drop(_log_guard);
        std::process::exit(code);
    }

    tauri::Builder::default()
        // Must be first: a second launch just brings the running Wren forward.
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if args.iter().any(|a| a == "--quit") {
                tracing::info!("asked to quit by a second launch (--quit)");
                app.exit(0);
                return;
            }
            // An autostart launch (`--hidden`) while Wren already runs must not pop the window up.
            if !args.iter().any(|a| a == "--hidden") {
                tray::show_main(app);
            }
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            if quit {
                // `--quit` with no Wren running: nothing to do.
                std::process::exit(0);
            }
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
            // Recorder, engine, hotkey and the session (needs AppState and both windows). The popup
            // check only exercises the window: no sidecar, no hook.
            if !popup_check {
                let services = session::app::start_services(app.handle())?;
                app.manage(services);
            }
            // Retention sweep now (off the main thread) and then every SWEEP_INTERVAL_MS.
            let handle = app.handle().clone();
            std::thread::Builder::new()
                .name("wren-sweep".into())
                .spawn(move || loop {
                    let state = handle.state::<AppState>();
                    sweep_store(&state.store, &state.paths);
                    std::thread::sleep(std::time::Duration::from_millis(constants::SWEEP_INTERVAL_MS));
                })?;
            if !start_hidden && !popup_check {
                if let Some(main) = app.get_webview_window(tray::MAIN_LABEL) {
                    main.show()?;
                }
            }
            if popup_check {
                // Exercise the popup from a worker thread (as the session will) and exit.
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let failures = popup::self_check(&handle);
                    if failures.is_empty() {
                        println!("POPUP CHECK PASS");
                        tracing::info!("popup check pass");
                        handle.exit(0);
                    } else {
                        for f in &failures {
                            println!("POPUP CHECK FAIL: {f}");
                        }
                        tracing::error!(?failures, "popup check failed");
                        handle.exit(1);
                    }
                });
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
            commands::popup_copy_text,
            commands::popup_undo_learned,
            commands::popup_discard,
            commands::popup_open_main,
            commands::popup_open_mic_settings,
            commands::engine_status,
            commands::engine_restart,
            commands::audio_devices,
            commands::set_audio_device,
            commands::other_apps_running,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Wren")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Unhook, stop the sidecar, put back a clipboard a paste still owes (paste::flush).
                if let Some(services) = app.try_state::<Services>() {
                    services.shutdown();
                }
                tracing::info!("Wren stopped");
            }
        });
}
