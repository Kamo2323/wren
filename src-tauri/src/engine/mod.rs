//! The speech engine (spec §5): a supervised Python sidecar running faster-whisper, spoken to over
//! JSON lines on stdio (`protocol`). `supervisor` owns the process: CREATE_NO_WINDOW, a Job Object
//! with KILL_ON_JOB_CLOSE (so closing or crashing Wren never leaves python.exe holding GPU memory),
//! stderr to `logs/sidecar.log`, restarts with back-off.
//!
//! Privacy: dictated text is only ever inside `TranscribeResult`/`Segment`, whose `Debug` output is
//! redacted. Nothing here logs it.

pub mod protocol;
mod supervisor;

use crate::paths::Paths;
use crate::settings::EngineSettings;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Overrides where the sidecar's Python package is found (tests, unusual installs).
pub const SIDECAR_DIR_ENV: &str = "WREN_SIDECAR_DIR";

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum EngineStatus {
    /// Launching the sidecar process (also while waiting to restart it).
    Starting,
    /// The process runs and is loading the model / transcribing the warm-up clip.
    WarmingUp,
    /// `device` is "cuda" or "cpu" (the CPU fallback, which the UI must announce).
    Ready { model: String, device: String, vram_mb: u64 },
    /// Gave up (too many failures) or shut down. `reason` is user-facing.
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscribeRequest {
    pub id: String,
    pub audio_path: PathBuf,
    pub bias_terms: Vec<String>,
    pub prompt: String,
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f32,
    pub end: f32,
    pub text: String,
    pub no_speech_prob: f32,
}

impl std::fmt::Debug for Segment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Segment")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("text_chars", &self.text.chars().count())
            .field("no_speech_prob", &self.no_speech_prob)
            .finish()
    }
}

#[derive(Clone, PartialEq)]
pub struct TranscribeResult {
    pub text: String,
    /// Speech the VAD kept, in ms (under `MIN_SPEECH_MS` = nothing to paste).
    pub speech_ms: u64,
    /// Time the sidecar spent on this request, in ms.
    pub infer_ms: u64,
    pub segments: Vec<Segment>,
}

impl std::fmt::Debug for TranscribeResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TranscribeResult")
            .field("text_chars", &self.text.chars().count())
            .field("speech_ms", &self.speech_ms)
            .field("infer_ms", &self.infer_ms)
            .field("segments", &self.segments)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    #[error("The speech engine is not ready yet.")]
    NotReady,
    #[error("The graphics card ran out of memory ({0}).")]
    Oom(String),
    #[error("The recording could not be read ({0}).")]
    BadAudio(String),
    #[error("The speech engine had a problem ({0}).")]
    Internal(String),
    #[error("The speech engine took too long to answer.")]
    Timeout,
}

/// Everything the supervisor needs to launch the sidecar.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// `runtime\.venv\Scripts\python.exe`.
    pub python: PathBuf,
    /// Folder holding the `wren_sidecar` package (and `assets\warmup.wav`).
    pub sidecar_dir: PathBuf,
    pub models_dir: PathBuf,
    pub warmup_wav: PathBuf,
    /// The sidecar's stderr is appended here.
    pub log_file: PathBuf,
    pub settings: EngineSettings,
}

impl EngineConfig {
    pub fn from_paths(paths: &Paths, settings: &EngineSettings) -> Self {
        let sidecar_dir = resolve_sidecar_dir();
        Self {
            python: paths.runtime_python(),
            warmup_wav: sidecar_dir.join("assets").join("warmup.wav"),
            sidecar_dir,
            models_dir: paths.models_dir(),
            log_file: paths.logs_dir().join("sidecar.log"),
            settings: settings.clone(),
        }
    }

    /// Command-line arguments after `python` (spec: the sidecar CLI).
    pub fn sidecar_args(&self, force_cpu: bool) -> Vec<String> {
        let s = &self.settings;
        let mut args = vec![
            "-s".into(),
            "-m".into(),
            "wren_sidecar".into(),
            "--models-dir".into(),
            self.models_dir.display().to_string(),
            "--model".into(),
            s.model.clone(),
            "--compute".into(),
            s.compute_type.clone(),
            "--beam".into(),
            s.beam_size.max(1).to_string(),
            "--cpu-fallback".into(),
            s.cpu_fallback_model.clone(),
            "--warmup-wav".into(),
            self.warmup_wav.display().to_string(),
            "--language".into(),
            s.language.clone(),
        ];
        if !s.vad_filter {
            args.push("--no-vad".into());
        }
        if s.condition_on_previous_text {
            args.push("--condition-on-previous-text".into());
        }
        if force_cpu {
            args.extend(["--device".into(), "cpu".into()]);
        }
        args
    }
}

/// Where the `wren_sidecar` package lives: `WREN_SIDECAR_DIR`, else next to the exe as a Tauri
/// resource (`../sidecar` is bundled as `_up_/sidecar`), else the source tree (dev builds).
pub fn resolve_sidecar_dir() -> PathBuf {
    let has_package = |d: &Path| d.join("wren_sidecar").join("__main__.py").is_file();
    if let Some(dir) = std::env::var_os(SIDECAR_DIR_ENV).map(PathBuf::from) {
        return dir;
    }
    let mut candidates = Vec::new();
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        candidates.push(exe_dir.join("_up_").join("sidecar"));
        candidates.push(exe_dir.join("sidecar"));
    }
    let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("sidecar");
    candidates.push(dev.clone());
    candidates.into_iter().find(|d| has_package(d)).unwrap_or(dev)
}

/// Handle to the supervised sidecar. Cheap to share behind an `Arc`; dropping it shuts the sidecar
/// down (and the Job Object guarantees the process dies even if Wren itself crashes).
pub struct Engine {
    inner: Arc<supervisor::Supervisor>,
}

impl Engine {
    /// Launch the sidecar in the background; progress arrives on `status_tx` (and `status()`).
    pub fn start(paths: &Paths, settings: &EngineSettings, status_tx: Sender<EngineStatus>) -> Self {
        Self::start_with(EngineConfig::from_paths(paths, settings), status_tx)
    }

    pub fn start_with(config: EngineConfig, status_tx: Sender<EngineStatus>) -> Self {
        Self { inner: supervisor::Supervisor::start(config, status_tx) }
    }

    /// Blocking: send one request and wait up to `timeout` for its result.
    pub fn transcribe(&self, req: TranscribeRequest, timeout: Duration) -> Result<TranscribeResult, EngineError> {
        self.inner.transcribe(req, timeout)
    }

    pub fn status(&self) -> EngineStatus {
        self.inner.status()
    }

    /// Kill the current sidecar (if any) and start a fresh one; also clears a `Failed` state.
    pub fn restart(&self) {
        self.inner.restart();
    }

    /// Ask the sidecar to exit, then make sure it has. Idempotent; also done on drop.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }

    /// True if the sidecar answered a ping within `timeout`.
    pub fn ping(&self, timeout: Duration) -> bool {
        self.inner.ping(timeout)
    }

    /// Process ids currently in the sidecar's Job Object: the venv launcher first, then the
    /// interpreter it started (and the console host Windows attaches). For diagnostics and tests.
    pub fn process_ids(&self) -> Vec<u32> {
        self.inner.process_ids()
    }
}

/// True if a process with this id is still running (used to prove no sidecar outlives Wren).
#[cfg(windows)]
pub fn process_is_running(pid: u32) -> bool {
    use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let alive = GetExitCodeProcess(h, &mut code).is_ok() && code == STILL_ACTIVE.0 as u32;
        let _ = CloseHandle(h);
        alive
    }
}

/// Full path of a running process's executable (diagnostics; tells the interpreter from the launcher).
#[cfg(windows)]
pub fn process_image(pid: u32) -> Option<PathBuf> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        ok.then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])))
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.inner.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_follow_settings() {
        let paths = Paths::under(Path::new("C:/tmp/wren-engine-test"));
        let cfg = EngineConfig::from_paths(&paths, &EngineSettings::default());
        let args = cfg.sidecar_args(false);
        let joined = args.join(" ");
        assert!(joined.starts_with("-s -m wren_sidecar --models-dir "));
        for want in ["--model large-v3-turbo", "--compute int8_float16", "--beam 1", "--cpu-fallback small.en", "--language en"] {
            assert!(joined.contains(want), "{want} in {joined}");
        }
        assert!(!joined.contains("--no-vad") && !joined.contains("--condition-on-previous-text"));
        assert!(!joined.contains("--device"));
        assert!(cfg.sidecar_args(true).join(" ").ends_with("--device cpu"));
        assert!(cfg.python.ends_with("python.exe") && cfg.log_file.ends_with("sidecar.log"));

        let mut s = EngineSettings::default();
        s.vad_filter = false;
        s.condition_on_previous_text = true;
        s.beam_size = 0;
        let joined = EngineConfig::from_paths(&paths, &s).sidecar_args(false).join(" ");
        assert!(joined.contains("--no-vad") && joined.contains("--condition-on-previous-text") && joined.contains("--beam 1"));
    }

    #[test]
    fn sidecar_dir_resolves_to_the_source_tree_in_dev() {
        let dir = resolve_sidecar_dir();
        assert!(dir.join("wren_sidecar").join("__main__.py").is_file(), "{}", dir.display());
    }

    #[test]
    fn status_serialises_for_the_ui() {
        let s = EngineStatus::Ready { model: "m".into(), device: "cpu".into(), vram_mb: 0 };
        assert_eq!(serde_json::to_string(&s).unwrap(), r#"{"state":"ready","model":"m","device":"cpu","vramMb":0}"#);
        assert_eq!(serde_json::to_string(&EngineStatus::WarmingUp).unwrap(), r#"{"state":"warmingUp"}"#);
    }

    #[test]
    fn debug_output_is_redacted() {
        let r = TranscribeResult {
            text: "private words".into(),
            speech_ms: 1,
            infer_ms: 1,
            segments: vec![Segment { start: 0.0, end: 1.0, text: "private words".into(), no_speech_prob: 0.0 }],
        };
        assert!(!format!("{r:?}").contains("private"));
    }
}
