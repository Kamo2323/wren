//! User settings, stored as versioned JSON at `%APPDATA%\Wren\settings.json` and written atomically.
//! Unknown or missing fields fall back to defaults, so old files keep loading.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct EngineSettings {
    /// faster-whisper model name or folder under `models_dir`.
    pub model: String,
    /// CTranslate2 compute type on the GPU.
    pub compute_type: String,
    pub beam_size: u32,
    /// Spoken language passed to the model (forced; skips language detection).
    pub language: String,
    pub vad_filter: bool,
    pub condition_on_previous_text: bool,
    /// Model used if CUDA is unavailable (runs on CPU, int8). Always announced to the user.
    pub cpu_fallback_model: String,
}

impl Default for EngineSettings {
    fn default() -> Self {
        #[cfg(target_os = "macos")]
        let (model, compute_type) = ("large-v3-turbo-q4", "q4");
        #[cfg(not(target_os = "macos"))]
        let (model, compute_type) = ("large-v3-turbo", "int8_float16");
        Self {
            model: model.into(),
            compute_type: compute_type.into(),
            beam_size: 1,
            language: "en".into(),
            vad_filter: true,
            condition_on_previous_text: false,
            cpu_fallback_model: "small.en".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AiCleanupSettings {
    pub enabled: bool,
    /// Ollama model tag.
    pub model: String,
}

impl Default for AiCleanupSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "qwen2.5:3b".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    pub engine: EngineSettings,
    /// Microphone device name; `None` follows the Windows default input.
    pub mic_device: Option<String>,
    /// Keep the mic stream open with a short pre-roll (only if first words get clipped).
    pub always_open_mic: bool,
    /// Typing speed (words per minute) used for "time saved".
    pub typing_wpm: u32,
    pub start_with_windows: bool,
    pub sounds: bool,
    pub ai_cleanup: AiCleanupSettings,
    /// Correction watcher: `None` = decided by the UI Automation probe result.
    pub watcher_enabled: Option<bool>,
    /// The Willow dictionary has been offered/imported once.
    pub willow_seed_imported: bool,
    /// Colour theme preference: `None` follows Windows, or `Some("dark")` / `Some("light")`.
    pub theme: Option<String>,
    /// Name on the user's own meeting lines ("Me"), meetings spec §5.3.
    pub meeting_name: String,
    /// Claude model for enhanced meeting notes (meetings spec §5.4).
    pub claude_model: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            engine: EngineSettings::default(),
            mic_device: None,
            always_open_mic: false,
            typing_wpm: crate::constants::TYPING_WPM_DEFAULT,
            start_with_windows: false,
            sounds: false,
            ai_cleanup: AiCleanupSettings::default(),
            watcher_enabled: None,
            willow_seed_imported: false,
            theme: None,
            meeting_name: "Maximilian".into(),
            claude_model: "claude-opus-5-5".into(),
        }
    }
}

impl Settings {
    /// Load from `path`; a missing or unreadable file gives defaults (a corrupt file is kept aside as `.bad`).
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(mut s) => {
                    s.version = SETTINGS_VERSION;
                    s
                }
                Err(err) => {
                    tracing::warn!(%err, "settings file unreadable; using defaults");
                    let _ = std::fs::rename(path, path.with_extension("json.bad"));
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                // Unreadable but present (permissions, bad encoding): keep a copy so a later save
                // cannot overwrite the only original.
                tracing::warn!(%err, "settings file unreadable; using defaults");
                let _ = std::fs::copy(path, path.with_extension("json.bad"));
                Self::default()
            }
        }
    }

    /// Write atomically: temp file in the same folder, then rename over the old one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        {
            use std::io::Write;
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?; // data on disk before the rename, so a power cut can't leave an empty file
        }
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A per-test folder under the system temp dir (removed again by `Cleanup`).
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("wren-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trips() {
        let dir = temp_dir("rt");
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.typing_wpm = 55;
        s.mic_device = Some("USB Mic".into());
        s.theme = Some("dark".into());
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = temp_dir("missing");
        let _cleanup = Cleanup(dir.clone());
        assert_eq!(Settings::load(&dir.join("nope.json")), Settings::default());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = temp_dir("partial");
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"typingWpm": 70}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.typing_wpm, 70);
        assert_eq!(s.engine, EngineSettings::default());
        assert!(!s.ai_cleanup.enabled);
        assert_eq!(s.theme, None);
    }

    #[test]
    fn corrupt_file_is_set_aside() {
        let dir = temp_dir("corrupt");
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("settings.json");
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        assert!(dir.join("settings.json.bad").exists());
    }
}
