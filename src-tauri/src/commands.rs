//! Tauri commands the front end calls. Later tasks add their commands here, one section each.

use crate::constants::{ui_constants, UiConstants};
use crate::settings::Settings;
use crate::AppState;
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
