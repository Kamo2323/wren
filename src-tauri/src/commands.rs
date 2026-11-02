//! Tauri commands the front end calls. Later tasks add their commands here, one section each.

use crate::constants::{ui_constants, UiConstants};
use crate::dictionary::{DictionaryEntry, Source, Status};
use crate::settings::Settings;
use crate::store::{Stats, TranscriptionRow};
use crate::AppState;
use serde::Serialize;
use tauri::{AppHandle, State};

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

#[tauri::command]
pub fn save_settings(state: State<'_, AppState>, settings: Settings) -> Result<(), String> {
    settings
        .save(&state.paths.settings_file())
        .map_err(|e| e.to_string())?;
    *state.settings.write() = settings;
    Ok(())
}

// ---- history, stats and dictionary (Task 4) ----
// Errors come back as the store's message (British English, no dictated text).

#[tauri::command]
pub fn history_recent(state: State<'_, AppState>, limit: u32) -> Result<Vec<TranscriptionRow>, String> {
    state.store.recent(limit).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_search(state: State<'_, AppState>, query: String) -> Result<Vec<TranscriptionRow>, String> {
    state.store.search(&query).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state.store.delete_transcription(&id).map_err(|e| e.to_string())
}

/// Returns how many dictations were deleted.
#[tauri::command]
pub fn history_delete_all(state: State<'_, AppState>) -> Result<usize, String> {
    state.store.delete_all_history().map_err(|e| e.to_string())
}

/// What "Fix a word" added: the replacement, and the term if it was new or switched back on.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixWordResult {
    pub replacement: DictionaryEntry,
    pub term: Option<DictionaryEntry>,
}

#[tauri::command]
pub fn history_fix_word(
    state: State<'_, AppState>,
    id: String,
    from: String,
    to: String,
) -> Result<FixWordResult, String> {
    let (replacement, term) = state.store.fix_word(&id, &from, &to).map_err(|e| e.to_string())?;
    Ok(FixWordResult { replacement, term })
}

#[tauri::command]
pub fn stats_get(state: State<'_, AppState>) -> Result<Stats, String> {
    let typing_wpm = state.settings.read().typing_wpm;
    state.store.stats(chrono::Local::now(), typing_wpm).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn dictionary_list(state: State<'_, AppState>) -> Result<Vec<DictionaryEntry>, String> {
    state.store.dictionary_list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn dictionary_add_term(state: State<'_, AppState>, word: String) -> Result<DictionaryEntry, String> {
    state.store.add_term(&word, Source::Manual).map_err(|e| e.to_string())
}

#[tauri::command]
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

#[tauri::command]
pub fn dictionary_set_status(state: State<'_, AppState>, id: i64, status: Status) -> Result<(), String> {
    state.store.set_status(id, status).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn dictionary_delete(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.delete_entry(id).map_err(|e| e.to_string())
}
