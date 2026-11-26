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

/// AI clean-up models (Settings → AI clean-up). Haiku is the default: fast and cheap.
pub const AI_CLEANUP_MODEL_HAIKU: &str = "claude-haiku-5-5";
pub const AI_CLEANUP_MODEL_SONNET: &str = "claude-sonnet-5-5";

/// How much AI clean-up may change text going into coding apps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CodingStyle {
    /// "Tidy + format only": words kept; punctuation, backticks, lists.
    Tidy,
    /// "Rewrite as a clear prompt": ums, repeats and false starts go; reordered; lists and
    /// backticks. Meaning kept, words not. An unknown value in the file reads as this.
    #[default]
    #[serde(other)]
    Rewrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AiCleanupSettings {
    pub enabled: bool,
    /// Claude model: [`AI_CLEANUP_MODEL_HAIKU`] or [`AI_CLEANUP_MODEL_SONNET`].
    pub model: String,
    pub coding_style: CodingStyle,
}

impl Default for AiCleanupSettings {
    fn default() -> Self {
        Self { enabled: false, model: AI_CLEANUP_MODEL_HAIKU.into(), coding_style: CodingStyle::Rewrite }
    }
}

impl AiCleanupSettings {
    /// A model Wren offers; anything else (e.g. the old Ollama tag "qwen2.5:3b") becomes Haiku.
    pub fn normalise(&mut self) {
        if self.model != AI_CLEANUP_MODEL_HAIKU && self.model != AI_CLEANUP_MODEL_SONNET {
            self.model = AI_CLEANUP_MODEL_HAIKU.into();
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
    /// The user's name: on their own meeting lines (meetings spec §5.3) and the Home greeting.
    /// Empty on a fresh profile (meeting lines then say "Me"); a name the user saved is kept.
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
            meeting_name: String::new(),
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
                    s.ai_cleanup.normalise();
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
    fn a_fresh_profile_assumes_no_name_and_a_saved_name_is_kept() {
        assert_eq!(Settings::default().meeting_name, "");
        let dir = temp_dir("name");
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"version":1,"meetingName":"Sam"}"#).unwrap();
        assert_eq!(Settings::load(&path).meeting_name, "Sam");
    }

    #[test]
    fn an_ollama_model_becomes_haiku_and_the_switch_is_kept() {
        let dir = temp_dir("ai");
        let _cleanup = Cleanup(dir.clone());
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"version":1,"aiCleanup":{"enabled":true,"model":"qwen2.5:3b"}}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.ai_cleanup, AiCleanupSettings { enabled: true, model: AI_CLEANUP_MODEL_HAIKU.into(), coding_style: CodingStyle::Rewrite });
        std::fs::write(&path, r#"{"aiCleanup":{"enabled":false,"model":"claude-sonnet-5-5","codingStyle":"tidy"}}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!((s.ai_cleanup.model.as_str(), s.ai_cleanup.coding_style), (AI_CLEANUP_MODEL_SONNET, CodingStyle::Tidy));
        // An unknown style doesn't throw the file away.
        std::fs::write(&path, r#"{"typingWpm":61,"aiCleanup":{"codingStyle":"poetry"}}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!((s.typing_wpm, s.ai_cleanup.coding_style), (61, CodingStyle::Rewrite));
        assert_eq!(serde_json::to_value(CodingStyle::Tidy).unwrap(), "tidy");
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
