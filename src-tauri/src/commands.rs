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
use tauri::{AppHandle, Emitter, State};

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

/// Saves, then applies what changed: start with Windows, the microphone, the engine.
#[tauri::command(async)]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    services: State<'_, Services>,
    settings: Settings,
) -> Result<(), String> {
    let old = state.settings.read().clone();
    settings
        .save(&state.paths.settings_file())
        .map_err(|e| e.to_string())?;
    *state.settings.write() = settings.clone();
    let mut problems = Vec::new();
    if old.start_with_windows != settings.start_with_windows {
        use tauri_plugin_autostart::ManagerExt;
        let launcher = app.autolaunch();
        let r = if settings.start_with_windows { launcher.enable() } else { launcher.disable() };
        if let Err(err) = r {
            tracing::warn!(error = %err, "cannot change start with Windows");
            problems.push("Wren couldn't change starting with Windows.".to_string());
        }
    }
    if old.mic_device != settings.mic_device
        && let Some(rec) = services.env.audio_recorder()
        && let Err(err) = rec.set_device(settings.mic_device.clone())
    {
        problems.push(err.to_string());
    }
    if old.engine != settings.engine {
        services.env.replace_engine(settings.engine.clone());
    }
    if problems.is_empty() { Ok(()) } else { Err(problems.join(" ")) }
}

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

/// "Learned X → Y · Undo": switch the learned replacement off (a switched-off rule is never
/// re-learned).
#[tauri::command(async)]
pub fn popup_undo_learned(state: State<'_, AppState>, entry_id: i64) -> Result<(), String> {
    state.store.set_status(entry_id, Status::Disabled).map_err(|e| e.to_string())
}

/// The × on the hands-free popup.
#[tauri::command]
pub fn popup_discard(services: State<'_, Services>) {
    services.session.send(Msg::Discard);
}

/// Error popup click: open Wren.
#[tauri::command]
pub fn popup_open_main(app: AppHandle) {
    crate::tray::show_main(&app);
}

#[tauri::command]
pub fn engine_status(services: State<'_, Services>) -> EngineStatus {
    services.env.engine().status()
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
    if let Some(rec) = services.env.audio_recorder() {
        rec.set_device(name.clone()).map_err(|e| e.to_string())?;
    }
    let mut settings = state.settings.read().clone();
    settings.mic_device = name;
    settings.save(&state.paths.settings_file()).map_err(|e| e.to_string())?;
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
        assert!(names.iter().any(|n| n.to_lowercase().ends_with(".exe")), "{} names", names.len());
    }
}
