//! Wren — local dictation for Windows. See docs/design/SPEC.md.

pub mod ai_cleanup;
pub mod app_icons;
pub mod audio;
pub mod commands;
pub mod constants;
pub mod dictionary;
pub mod engine;
pub mod hotkey;
pub mod logging;
pub mod meetings;
pub mod paste;
pub mod paths;
pub mod pipeline;
pub mod popup;
pub mod selftest;
pub mod session;
pub mod settings;
pub mod store;
pub mod tray;
pub mod updater;
pub mod watcher;

use parking_lot::RwLock;
use std::sync::Arc;
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
    pub store: Arc<Store>,
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
    match store.meeting_sweep(chrono::Utc::now(), &paths.meetings_dir()) {
        Ok(n) => tracing::info!(meetings_audio_deleted = n, "meeting retention sweep"),
        Err(err) => tracing::warn!(error = %err, "meeting retention sweep failed"),
    }
}

/// Colour the main window's chrome/caption to match the active Light or Dark theme (`None` /
/// `"system"` follows the OS). Windows 11 uses DWM attributes; macOS uses `NSWindow` appearance.
pub fn apply_window_theme(win: &tauri::WebviewWindow, theme: Option<&str>) {
    let dark = match theme.map(|s| s.to_ascii_lowercase()).as_deref() {
        Some("dark") => {
            let _ = win.set_theme(Some(tauri::Theme::Dark));
            true
        }
        Some("light") => {
            let _ = win.set_theme(Some(tauri::Theme::Light));
            false
        }
        _ => {
            let _ = win.set_theme(None);
            matches!(win.theme(), Ok(tauri::Theme::Dark))
        }
    };

    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{COLORREF, HWND};
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
        };

        let Ok(h) = win.hwnd() else { return };
        let hwnd = HWND(h.0 as *mut core::ffi::c_void);
        let set_color = |attr, colour: u32| unsafe {
            let value = COLORREF(colour);
            DwmSetWindowAttribute(
                hwnd,
                attr,
                &value as *const COLORREF as *const core::ffi::c_void,
                std::mem::size_of::<COLORREF>() as u32,
            )
        };
        let dark_flag: i32 = if dark { 1 } else { 0 };
        let _ = unsafe {
            DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_flag as *const i32 as *const core::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            )
        };

        // COLORREF is 0x00BBGGRR. Dark matches --ground (#1c232d) / --line (#323e4b) / --ink (#f2f4f7);
        // Light matches --ground (#fbfcff) / --line (#e1e6ee) / --ink (#1b2738).
        let (caption, border, text) = if dark {
            (0x002D_231C, 0x004B_3E32, 0x00F7_F4F2)
        } else {
            (0x00FF_FCFB, 0x00EE_E6E1, 0x0038_271B)
        };
        let results = [
            set_color(DWMWA_CAPTION_COLOR, caption),
            set_color(DWMWA_BORDER_COLOR, border),
            set_color(DWMWA_TEXT_COLOR, text),
        ];
        if let Some(Err(err)) = results.into_iter().find(|r| r.is_err()) {
            tracing::info!(error = %err, "caption colour not supported (Windows 10?); using window theme only");
        }
    }
    #[cfg(not(windows))]
    {
        let _ = dark;
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

    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.activate_ignoring_other_apps(false);

    builder
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
            // A meeting still `recording`/`paused` in the database means Wren quit or crashed mid-meeting
            // (meetings spec §5.5): it becomes `interrupted`, keeping everything transcribed so far.
            if store.unavailable_reason().is_none() {
                match store.mark_interrupted() {
                    Ok(0) => {}
                    Ok(n) => tracing::warn!(meetings = n, "meetings interrupted by the last shutdown"),
                    Err(err) => tracing::warn!(error = %err, "could not mark interrupted meetings"),
                }
                // A crash leaves WAV headers up to 10 s behind the audio: fix them so ▶ plays everything.
                for id in store.interrupted_with_audio().unwrap_or_default() {
                    meetings::capture::wav::repair_meeting_audio(&paths.meeting_dir(&id));
                }
            }
            let updates_dir = paths.updates_dir();
            let logs_dir = paths.logs_dir();
            app.manage(AppState {
                paths,
                settings: RwLock::new(settings),
                store: Arc::new(store),
            });
            app.manage(updater::UpdaterService::new(
                app.package_info().version.to_string(),
                updates_dir,
                logs_dir,
            ));
            // Neither is fatal (M-7): without the tray or the popup Wren still dictates and pastes
            // (the popup helpers no-op without a window). The popup check needs the popup.
            if let Err(err) = tray::build(app.handle()) {
                tracing::error!(error = %err, "cannot create the tray icon; continuing without it");
            }
            if let Err(err) = popup::create(app.handle()) {
                if popup_check {
                    return Err(err.into());
                }
                tracing::error!(error = %err, "cannot create the popup window; continuing without it");
            }
            // Recorder, engine, hotkey and the session (needs AppState and both windows). The popup
            // check only exercises the window: no sidecar, no hook.
            if !popup_check {
                let services = session::app::start_services(app.handle())?;
                let meetings = meetings::commands::build_service(app.handle(), services.env.clone());
                app.manage(meetings::commands::Meetings(meetings));
                app.manage(services);
                updater::UpdaterService::start_background(app.handle().clone());
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
                let theme = app.state::<AppState>().settings.read().theme.clone();
                apply_window_theme(&main, theme.as_deref());
            }
            if start_hidden || popup_check {
                #[cfg(target_os = "macos")]
                let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            } else {
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
            // Closing the main window hides it to the tray/menu bar; Quit lives in the tray menu.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == tray::MAIN_LABEL {
                    api.prevent_close();
                    let _ = window.hide();
                    #[cfg(target_os = "macos")]
                    let _ = window.app_handle().set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_constants,
            commands::app_version,
            commands::get_settings,
            commands::save_settings,
            commands::set_window_theme,
            commands::app_icon,
            commands::accessibility_status,
            commands::open_accessibility_settings,
            commands::history_recent,
            commands::history_search,
            commands::history_delete,
            commands::history_delete_all,
            commands::history_fix_word,
            commands::history_audio_ids,
            commands::history_audio,
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
            meetings::commands::meeting_start,
            meetings::commands::meeting_pause,
            meetings::commands::meeting_resume,
            meetings::commands::meeting_stop,
            meetings::commands::meeting_stop_and_quit,
            meetings::commands::meeting_active,
            meetings::commands::meeting_list,
            meetings::commands::meeting_get,
            meetings::commands::meeting_update,
            meetings::commands::meeting_rename_speaker,
            meetings::commands::meeting_delete,
            meetings::commands::meeting_audio_clip,
            meetings::commands::system_audio_permission,
            meetings::commands::open_system_audio_settings,
            meetings::commands::meeting_enhance,
            meetings::commands::meeting_generating,
            meetings::commands::claude_key_status,
            meetings::commands::claude_key_save,
            meetings::commands::claude_key_remove,
            meetings::commands::claude_spend_month,
            commands::popup_open_mic_settings,
            commands::engine_status,
            commands::engine_restart,
            commands::audio_devices,
            commands::set_audio_device,
            commands::other_apps_running,
            commands::watcher_status,
            commands::ai_cleanup_status,
            commands::ai_cleanup_pull_model,
            commands::update_status,
            commands::update_check,
            commands::update_install,
        ])
        .build(tauri::generate_context!())
        .expect("error while building Wren")
        .run(move |app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Ready = event
                && (start_hidden || popup_check)
            {
                popup::prime_hidden_launch(app);
            }
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                let main_visible = app
                    .get_webview_window(tray::MAIN_LABEL)
                    .is_some_and(|w| w.is_visible().unwrap_or(false));
                if !main_visible {
                    tray::show_main(app);
                }
            }
            // Cmd+Q, Dock → Quit, the app menu: with a meeting recording, ask first (spec §5.5).
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                let recording = app.try_state::<meetings::commands::Meetings>().is_some_and(|m| m.0.active().is_some());
                if recording && !meetings::commands::QUIT_CONFIRMED.load(std::sync::atomic::Ordering::SeqCst) {
                    api.prevent_exit();
                    tray::show_main(app);
                    let _ = tauri::Emitter::emit(app, meetings::commands::EVENT_CONFIRM_QUIT, ());
                }
            }
            if let tauri::RunEvent::Exit = event {
                // A meeting still recording ends properly (short drain, no paid notes request) rather
                // than "interrupted".
                if let Some(m) = app.try_state::<meetings::commands::Meetings>() {
                    m.0.stop_for_quit(std::time::Duration::from_secs(3));
                }
                // Unhook, stop the sidecar, put back a clipboard a paste still owes (paste::flush).
                if let Some(services) = app.try_state::<Services>() {
                    services.shutdown();
                }
                tracing::info!("Wren stopped");
            }
        });
}
