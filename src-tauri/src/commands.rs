//! Tauri commands the front end calls. Later tasks add their commands here, one section each.

use crate::constants::{ui_constants, UiConstants};
use crate::dictionary::{DictionaryEntry, Source, Status};
use crate::engine::EngineStatus;
use crate::session::app::{Services, EVENT_HISTORY_CHANGED};
use crate::session::Msg;
use crate::settings::Settings;
use crate::store::{Stats, TranscriptionRow};
use crate::AppState;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

fn history_changed(app: &AppHandle) {
    let _ = app.emit(EVENT_HISTORY_CHANGED, ());
}

// ---- app ----

#[tauri::command]
pub fn get_constants() -> UiConstants {
    ui_constants()
}

#[tauri::command]
pub fn app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

// ---- settings ----

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Settings {
    state.settings.read().clone()
}

/// One settings write at a time (`save_settings` and `set_audio_device`).
static SETTINGS_WRITE: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

fn set_autostart(app: &AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let launcher = app.autolaunch();
    let r = if on { launcher.enable() } else { launcher.disable() };
    r.map_err(|err| {
        tracing::warn!(error = %err, "cannot change start at login");
        #[cfg(target_os = "macos")]
        {
            "Wren couldn't change starting at login.".to_string()
        }
        #[cfg(not(target_os = "macos"))]
        {
            "Wren couldn't change starting with Windows.".to_string()
        }
    })
}

/// Applies what changed (start with Windows, the microphone), then saves. If any step fails the
/// earlier ones are undone and nothing is saved, so the next save tries again. A changed engine is
/// restarted after the save.
#[tauri::command(async)]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    services: State<'_, Services>,
    settings: Settings,
) -> Result<(), String> {
    let _one_writer = SETTINGS_WRITE.lock();
    let old = state.settings.read().clone();
    let autostart_changed = old.start_with_windows != settings.start_with_windows;
    let mic_changed = old.mic_device != settings.mic_device;
    let recorder = services.env.audio_recorder();
    if autostart_changed {
        set_autostart(&app, settings.start_with_windows)?;
    }
    let undo = |mic: bool| {
        if autostart_changed {
            let _ = set_autostart(&app, old.start_with_windows);
        }
        if mic && let Some(rec) = recorder {
            let _ = rec.set_device(old.mic_device.clone());
        }
    };
    if mic_changed
        && let Some(rec) = recorder
        && let Err(err) = rec.set_device(settings.mic_device.clone())
    {
        undo(false);
        return Err(err.to_string());
    }
    if let Err(err) = settings.save(&state.paths.settings_file()) {
        undo(mic_changed);
        return Err(err.to_string());
    }
    *state.settings.write() = settings.clone();
    if old.theme != settings.theme
        && let Some(main) = app.get_webview_window(crate::tray::MAIN_LABEL)
    {
        crate::apply_window_theme(&main, settings.theme.as_deref());
    }
    if old.engine != settings.engine {
        services.env.replace_engine(settings.engine.clone());
    }
    Ok(())
}

/// Apply Light / Dark / System to the native Windows titlebar immediately.
#[tauri::command]
pub fn set_window_theme(app: AppHandle, theme: Option<String>) {
    if let Some(main) = app.get_webview_window(crate::tray::MAIN_LABEL) {
        crate::apply_window_theme(&main, theme.as_deref());
    }
}

/// Extract and cache a real OS app icon PNG data URL for a process/app name.
#[tauri::command(async)]
pub fn app_icon(process_name: String) -> Option<String> {
    crate::app_icons::icon_data_url(&process_name)
}

/// On macOS, returns whether Accessibility permission is granted (needed for Fn + Control, ⌘V paste,
/// and the correction watcher). If permission was just granted after launch, installs the hotkey
/// immediately so no restart is needed. Always `true` on Windows.
#[tauri::command]
pub fn accessibility_status(app: AppHandle) -> bool {
    #[cfg(target_os = "macos")]
    {
        let trusted = crate::hotkey::accessibility_trusted(false);
        if trusted
            && let Some(services) = app.try_state::<Services>()
            && !services.env.has_hotkey()
        {
            services.session.send(Msg::ReinstallHook);
        }
        trusted
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        true
    }
}

/// On macOS, prompts for Accessibility (if needed) and opens System Settings → Privacy & Security →
/// Accessibility. Also reinstalls the hotkey if permission is already granted.
#[tauri::command]
pub fn open_accessibility_settings(app: AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        if crate::hotkey::accessibility_trusted(true) {
            if let Some(services) = app.try_state::<Services>() {
                services.session.send(Msg::ReinstallHook);
            }
            return Ok(());
        }
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_url(ACCESSIBILITY_SETTINGS_URI, None::<&str>)
            .map_err(|err| {
                tracing::warn!(error = %err, "cannot open Accessibility settings");
                "Couldn't open Accessibility settings.".to_string()
            })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
pub const ACCESSIBILITY_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

// ---- history, stats and dictionary (Task 4) ----
// Errors come back as the store's message (British English, no dictated text).
// `async` runs them on Tauri's worker pool, so database and file work (e.g. delete-all removing
// 72 h of WAVs under the store's lock) never blocks the main event loop.

#[tauri::command(async)]
pub fn history_recent(state: State<'_, AppState>, limit: u32) -> Result<Vec<TranscriptionRow>, String> {
    state.store.recent(limit).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn history_search(state: State<'_, AppState>, query: String) -> Result<Vec<TranscriptionRow>, String> {
    state.store.search(&query).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn history_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.delete_transcription(&id).map_err(|e| e.to_string())?;
    history_changed(&app);
    Ok(())
}

/// Returns how many dictations were deleted.
#[tauri::command(async)]
pub fn history_delete_all(app: AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    let n = state.store.delete_all_history().map_err(|e| e.to_string())?;
    history_changed(&app);
    Ok(n)
}

/// What "Fix a word" added: the replacement, and the term if it was new or switched back on.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixWordResult {
    pub replacement: DictionaryEntry,
    pub term: Option<DictionaryEntry>,
}

#[tauri::command(async)]
pub fn history_fix_word(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    from: String,
    to: String,
) -> Result<FixWordResult, String> {
    let (replacement, term) = state.store.fix_word(&id, &from, &to).map_err(|e| e.to_string())?;
    history_changed(&app);
    Ok(FixWordResult { replacement, term })
}

/// Returns the subset of transcription ids whose local WAV recording currently exists on disk.
#[tauri::command(async)]
pub fn history_audio_ids(state: State<'_, AppState>, ids: Vec<String>) -> Result<Vec<String>, String> {
    state.store.ids_with_audio(&ids).map_err(|e| e.to_string())
}

/// Reads the local WAV recording for one dictation as a `data:audio/wav;base64,...` URL, if present.
#[tauri::command(async)]
pub fn history_audio(state: State<'_, AppState>, id: String) -> Result<Option<String>, String> {
    use base64::Engine;
    let Some(path) = state.store.audio_path(&id).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|_| "Couldn't read that recording.".to_string())?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Some(format!("data:audio/wav;base64,{b64}")))
}

#[tauri::command(async)]
pub fn stats_get(state: State<'_, AppState>) -> Result<Stats, String> {
    let typing_wpm = state.settings.read().typing_wpm;
    state.store.stats(chrono::Local::now(), typing_wpm).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn dictionary_list(state: State<'_, AppState>) -> Result<Vec<DictionaryEntry>, String> {
    state.store.dictionary_list().map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn dictionary_add_term(state: State<'_, AppState>, word: String) -> Result<DictionaryEntry, String> {
    state.store.add_term(&word, Source::Manual).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn dictionary_add_replacement(
    state: State<'_, AppState>,
    from: String,
    to: String,
) -> Result<DictionaryEntry, String> {
    state
        .store
        .add_replacement(&from, &to, Source::Manual, Status::Active)
        .map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn dictionary_set_status(state: State<'_, AppState>, id: i64, status: Status) -> Result<(), String> {
    state.store.set_status(id, status).map_err(|e| e.to_string())
}

#[tauri::command(async)]
pub fn dictionary_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.delete_entry(id).map_err(|e| e.to_string())
}

// ---- popup, engine, audio, other apps (Task 8) ----

/// "Not pasted — click to copy".
#[tauri::command(async)]
pub fn popup_copy_text(text: String) -> Result<(), String> {
    crate::paste::copy_text(&text).map_err(|err| {
        tracing::warn!(error = %err, "popup copy failed");
        "Couldn't copy it. Open Wren to copy.".to_string()
    })
}

/// Copy from the main window (Home/History cards). Same as the popup's copy — plain text marked
/// out of clipboard history and cloud clipboard — never the WebView's clipboard API (I-2).
#[tauri::command(async)]
pub fn copy_text(text: String) -> Result<(), String> {
    crate::paste::copy_text(&text).map_err(|err| {
        tracing::warn!(error = %err, "copy failed");
        "Couldn't copy.".to_string()
    })
}

/// "Learned X → Y · Undo": switch off what that learn added — the replacement and a term created
/// with it (a term that was already there stays). A switched-off rule is never re-learned.
#[tauri::command(async)]
pub fn popup_undo_learned(app: AppHandle, state: State<'_, AppState>, entry_id: i64) -> Result<(), String> {
    state.store.undo_learned(entry_id).map_err(|e| e.to_string())?;
    history_changed(&app);
    Ok(())
}

/// The × on the hands-free popup.
#[tauri::command]
pub fn popup_discard(services: State<'_, Services>) {
    services.session.send(Msg::Discard);
}

/// "No sound from your mic" click: Windows' microphone privacy page (spec §4).
#[tauri::command]
pub fn popup_open_mic_settings(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(MIC_SETTINGS_URI, None::<&str>).map_err(|err| {
        tracing::warn!(error = %err, "cannot open the microphone settings");
        "Couldn't open the microphone settings.".to_string()
    })
}

/// OS Microphone privacy settings URI.
#[cfg(target_os = "macos")]
pub const MIC_SETTINGS_URI: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";
#[cfg(not(target_os = "macos"))]
pub const MIC_SETTINGS_URI: &str = "ms-settings:privacy-microphone";

/// The popup reports where the pill is (logical px inside the popup window), so only the pill takes
/// clicks in the clickable states.
#[tauri::command]
pub fn popup_hit_rect(x: f64, y: f64, w: f64, h: f64) {
    crate::popup::set_hit_rect(crate::popup::HitRect { x, y, w, h });
}

/// The main window's page has painted its first screen: show the window now (once).
#[tauri::command]
pub fn main_ready(app: AppHandle) {
    crate::tray::first_show(&app);
}

/// Error popup click: open Wren.
#[tauri::command]
pub fn popup_open_main(app: AppHandle) {
    crate::tray::show_main(&app);
}

/// Through the session's view (M-5): "Starting" while Settings swaps the engine, never the old
/// engine's "Failed: shut down".
#[tauri::command]
pub fn engine_status(services: State<'_, Services>) -> EngineStatus {
    crate::session::Env::engine_status(services.env.as_ref())
}

#[tauri::command(async)]
pub fn engine_restart(services: State<'_, Services>) {
    services.env.engine().restart();
}

#[tauri::command(async)]
pub fn audio_devices() -> Vec<String> {
    crate::audio::list_input_devices()
}

/// `None` follows the Windows default microphone. Applied at once and saved.
#[tauri::command(async)]
pub fn set_audio_device(
    state: State<'_, AppState>,
    services: State<'_, Services>,
    name: Option<String>,
) -> Result<(), String> {
    let _one_writer = SETTINGS_WRITE.lock();
    let mut settings = state.settings.read().clone();
    let old = settings.mic_device.clone();
    let recorder = services.env.audio_recorder();
    if let Some(rec) = recorder {
        rec.set_device(name.clone()).map_err(|e| e.to_string())?;
    }
    settings.mic_device = name;
    if let Err(err) = settings.save(&state.paths.settings_file()) {
        if let Some(rec) = recorder {
            let _ = rec.set_device(old);
        }
        return Err(err.to_string());
    }
    *state.settings.write() = settings;
    Ok(())
}

/// Other dictation apps running now ("Willow", "Glaido"): both pasting would double the text.
#[tauri::command(async)]
pub fn other_apps_running() -> Vec<String> {
    other_apps_in(running_process_names())
}

/// Maps process file names to the apps the banner names, each once, Willow first.
pub fn other_apps_in(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let (mut willow, mut glaido) = (false, false);
    for n in names {
        let n = n.to_lowercase();
        willow |= n.contains("willow");
        glaido |= n.contains("glaido");
    }
    let mut out = Vec::new();
    if willow {
        out.push("Willow".to_string());
    }
    if glaido {
        out.push("Glaido".to_string());
    }
    out
}

#[cfg(windows)]
fn running_process_names() -> Vec<String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let mut names = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return names;
        };
        let mut pe = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut pe).is_ok();
        while more {
            let len = pe.szExeFile.iter().position(|c| *c == 0).unwrap_or(pe.szExeFile.len());
            names.push(String::from_utf16_lossy(&pe.szExeFile[..len]));
            more = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    names
}

#[cfg(target_os = "macos")]
fn running_process_names() -> Vec<String> {
    use objc2_app_kit::NSWorkspace;
    let mut names = Vec::new();
    let apps = NSWorkspace::sharedWorkspace().runningApplications();
    for app in apps.iter() {
        if let Some(name) = app.localizedName() {
            names.push(name.to_string());
        }
        if let Some(exe) = app.executableURL().and_then(|u| u.lastPathComponent()) {
            names.push(exe.to_string());
        }
    }
    names
}

// ---- correction watcher, AI clean-up (Task 9) ----

/// Whether the correction watcher runs (the setting, or the Claude-app probe's default) and why.
#[tauri::command]
pub fn watcher_status(state: State<'_, AppState>, services: State<'_, Services>) -> crate::watcher::WatcherStatus {
    let setting = state.settings.read().watcher_enabled;
    crate::watcher::status(setting, &services.env.watcher().slow_apps())
}

/// Is Ollama running, and is the clean-up model downloaded?
#[tauri::command(async)]
pub fn ai_cleanup_status(state: State<'_, AppState>) -> crate::ai_cleanup::AiCleanupStatus {
    let model = state.settings.read().ai_cleanup.model.clone();
    crate::ai_cleanup::status(&model)
}

/// Downloads the clean-up model with `ollama pull`; returns when it has finished.
#[tauri::command(async)]
pub fn ai_cleanup_pull_model(state: State<'_, AppState>) -> Result<(), String> {
    let model = state.settings.read().ai_cleanup.model.clone();
    crate::ai_cleanup::pull(&model)
}

// ---- app updater ----

#[tauri::command]
pub fn update_status(updater: State<'_, crate::updater::UpdaterService>) -> crate::updater::UpdateStatus {
    updater.current_status()
}

#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    updater: State<'_, crate::updater::UpdaterService>,
) -> Result<crate::updater::UpdateStatus, String> {
    Ok(updater.check(&app, true).await)
}

#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    updater: State<'_, crate::updater::UpdaterService>,
) -> Result<(), String> {
    updater.install(&app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_apps_are_named_once_each() {
        let names = ["Willow Voice.exe", "glaido-core.exe", "Glaido.exe", "willow voice.exe", "msedge.exe"].map(String::from);
        assert_eq!(other_apps_in(names), vec!["Willow".to_string(), "Glaido".to_string()]);
        assert_eq!(other_apps_in(["glaido-core.exe".to_string()]), vec!["Glaido".to_string()]);
        assert!(other_apps_in(["wren.exe".to_string(), "claude.exe".to_string()]).is_empty());
    }

    #[test]
    fn the_process_list_can_be_read() {
        let names = running_process_names();
        #[cfg(windows)]
        assert!(names.iter().any(|n| n.to_lowercase().ends_with(".exe")), "{} names", names.len());
        #[cfg(target_os = "macos")]
        assert!(!names.is_empty(), "expected at least one running macOS application");
    }
}
