//! Wren — local dictation for Windows. See docs/design/SPEC.md.

pub mod ai_cleanup;
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
pub mod watcher;

use parking_lot::RwLock;
use paths::Paths;
use session::app::Services;
use settings::Settings;
use store::Store;
use tauri::Manager;

/// Marker for this build (`WREN_BUILD_ID`, set by scripts/release.ps1 and scripts/live-test.ps1;
/// "dev" otherwise). The scripts search the exe for this exact text to prove they are running the
/// build they just made, not one another build left in the shared target folder.
pub const BUILD_ID: &str = concat!("wren-build-id:", env!("WREN_BUILD_ID"));

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

/// Colour the main window's caption bar like the canvas (#f3f3f5) with dark text (#24232b), so it
/// reads as one surface with the sidebar even when Windows is in dark mode. Windows 11 only; on
/// Windows 10 the call fails and the window's light theme alone gives a light bar.
fn light_caption(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR};
    let Ok(h) = win.hwnd() else { return };
    let hwnd = HWND(h.0 as *mut core::ffi::c_void);
    let set = |attr, colour: u32| unsafe {
        let value = COLORREF(colour);
        DwmSetWindowAttribute(hwnd, attr, &value as *const COLORREF as *const core::ffi::c_void, std::mem::size_of::<COLORREF>() as u32)
    };
    // COLORREF is 0x00BBGGRR.
    let results = [
        set(DWMWA_CAPTION_COLOR, 0x00F5_F3F3),
        set(DWMWA_BORDER_COLOR, 0x00F5_F3F3),
        set(DWMWA_TEXT_COLOR, 0x002B_2324),
    ];
    if let Some(Err(err)) = results.into_iter().find(|r| r.is_err()) {
        tracing::info!(error = %err, "caption colour not supported (Windows 10?); using the light theme only");
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
    if quit {
        // A release build has no console; print into the terminal that ran `wren --quit`, if any.
        selftest::attach_console();
        println!("Wren: asking the running Wren to quit");
    }
    tracing::info!(version = env!("CARGO_PKG_VERSION"), build = BUILD_ID, start_hidden, selftest, quit, "Wren starting");
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
                println!("Wren: no Wren was running");
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
            if let Some(main) = app.get_webview_window(tray::MAIN_LABEL) {
                light_caption(&main);
            }
            if !start_hidden && !popup_check {
                // Shown once the page has painted (it calls `main_ready`), so the first thing seen is
                // the finished Home page, not an empty canvas filling in. A fallback shows it anyway.
                tray::arm_first_show();
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(constants::MAIN_SHOW_FALLBACK_MS));
                    let h = handle.clone();
                    let _ = handle.run_on_main_thread(move || tray::first_show(&h));
                });
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
            commands::copy_text,
            commands::popup_undo_learned,
            commands::popup_discard,
            commands::popup_open_main,
            commands::popup_hit_rect,
            commands::main_ready,
            commands::popup_open_mic_settings,
            commands::engine_status,
            commands::engine_restart,
            commands::audio_devices,
            commands::set_audio_device,
            commands::other_apps_running,
            commands::watcher_status,
            commands::ai_cleanup_status,
            commands::ai_cleanup_pull_model,
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
