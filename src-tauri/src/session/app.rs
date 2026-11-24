//! The real [`Env`]: Tauri (events, managed state), the microphone, the speech engine, the hotkey,
//! the popup window and the clipboard. `start_services` builds and starts all of it from `setup`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crossbeam_channel::Sender;
use parking_lot::{Mutex, RwLock};
use tauri::{AppHandle, Emitter, Listener, Manager};

use super::{Env, HotkeyCmd, Msg, PastedDictation, PopupState, Recorder, SessionHandle, Sound};
use crate::AppState;
use crate::audio::AudioRecorder;
use crate::constants::{
    ENGINE_SAMPLE_RATE, QUIT_DRAIN_MAX_MS, SOUND_START_HZ, SOUND_STOP_HZ, SOUND_TICK_AMPLITUDE, SOUND_TICK_MS,
    STARTUP_ERROR_POPUP_DELAY_MS,
};
use crate::engine::{Engine, EngineError, EngineStatus, TranscribeRequest, TranscribeResult};
use crate::hotkey::{HotkeyEvent, HotkeyService};
use crate::paste::{self, ForegroundInfo, PasteResult};
use crate::settings::{EngineSettings, Settings};
use crate::store::Store;
use crate::watcher::{self, Watcher, prepaste};
use crate::{ai_cleanup, popup, tray};

/// Payload: `EngineStatus`, on every change.
pub const EVENT_ENGINE_STATUS: &str = "app://engine-status";
/// No payload: History rows were added, changed or deleted.
pub const EVENT_HISTORY_CHANGED: &str = "app://history-changed";

pub struct AppEnv {
    app: AppHandle,
    recorder: Option<Arc<AudioRecorder>>,
    /// Replaced (not restarted) when the engine settings change.
    engine: RwLock<Arc<Engine>>,
    status_tx: Sender<EngineStatus>,
    /// One engine replacement at a time (shutdown + start + swap under one lock)…
    engine_swap: Mutex<()>,
    /// …and while it runs the engine reports `Starting`, so a finishing dictation waits for the new
    /// engine instead of failing on the old one.
    swapping: AtomicBool,
    hotkey: Mutex<Option<HotkeyService>>,
    /// Kept so `Reinstall` can start the hotkey after launch (e.g. once macOS Accessibility is granted).
    hotkey_tx: Sender<HotkeyEvent>,
    /// Wren's own window handles, read once on the main thread at start-up.
    #[cfg(windows)]
    own_windows: Vec<isize>,
    /// Correction watcher and pre-paste check (Task 9).
    watcher: Watcher,
    prepaste: Option<prepaste::Checker>,
    /// Set once the session has started (the watcher shows its toast through it).
    session: OnceLock<SessionHandle>,
    /// An AI clean-up model load is in flight.
    ai_warming: Arc<AtomicBool>,
    /// Stops the macOS post-launch Accessibility watcher thread on quit.
    #[cfg(target_os = "macos")]
    ax_watch_stop: Arc<AtomicBool>,
}

impl AppEnv {
    fn state(&self) -> &AppState {
        self.app.state::<AppState>().inner()
    }

    pub fn engine(&self) -> Arc<Engine> {
        self.engine.read().clone()
    }

    pub fn audio_recorder(&self) -> Option<&AudioRecorder> {
        self.recorder.as_deref()
    }

    pub fn watcher(&self) -> &Watcher {
        &self.watcher
    }

    pub fn has_hotkey(&self) -> bool {
        self.hotkey.lock().is_some()
    }

    /// New engine settings: shut the old sidecar down (frees the GPU), start a new one. In the
    /// background; dictations meanwhile wait for it like a warm-up (the status reads `Starting`).
    pub fn replace_engine(self: &Arc<Self>, settings: EngineSettings) {
        let me = self.clone();
        let spawned = std::thread::Builder::new().name("wren-engine-replace".into()).spawn(move || {
            let _one_at_a_time = me.engine_swap.lock();
            tracing::info!("engine settings changed; restarting the speech engine");
            me.swapping.store(true, Ordering::SeqCst);
            me.engine().shutdown();
            // After the old engine's last status, so the UI shows the gap as starting.
            let _ = me.status_tx.send(EngineStatus::Starting);
            let paths = me.state().paths.clone();
            let fresh = Arc::new(Engine::start(&paths, &settings, me.status_tx.clone()));
            *me.engine.write() = fresh;
            me.swapping.store(false, Ordering::SeqCst);
        });
        if let Err(err) = spawned {
            tracing::error!(error = %err, "cannot restart the speech engine");
        }
    }

    /// On quit: unhook, stop the sidecar, and put back any clipboard a paste still owes.
    pub fn shutdown(&self) {
        #[cfg(target_os = "macos")]
        self.ax_watch_stop.store(true, Ordering::SeqCst);
        self.watcher.stop();
        drop(self.hotkey.lock().take());
        self.engine().shutdown();
        paste::flush();
    }
}

impl Env for AppEnv {
    fn store(&self) -> &Store {
        &self.state().store
    }
    fn settings(&self) -> Settings {
        self.state().settings.read().clone()
    }
    fn audio_dir(&self) -> PathBuf {
        self.state().paths.audio_dir()
    }
    fn recorder(&self) -> Option<&dyn Recorder> {
        self.recorder.as_deref().map(|r| r as &dyn Recorder)
    }
    fn engine_status(&self) -> EngineStatus {
        if self.swapping.load(Ordering::SeqCst) { EngineStatus::Starting } else { self.engine().status() }
    }
    fn transcribe(&self, req: TranscribeRequest, timeout: Duration) -> Result<TranscribeResult, EngineError> {
        self.engine().transcribe(req, timeout)
    }
    fn foreground(&self) -> ForegroundInfo {
        paste::foreground()
    }
    fn paste(&self, text: &str, target: &ForegroundInfo) -> PasteResult {
        #[cfg(windows)]
        {
            paste::paste_text(text, target, &|hwnd| self.own_windows.contains(&hwnd))
        }
        #[cfg(target_os = "macos")]
        {
            paste::paste_text(text, target, &|hwnd| popup::is_own_window(&self.app, hwnd))
        }
    }
    fn copy_text(&self, text: &str) -> anyhow::Result<()> {
        paste::copy_text(text)
    }
    fn popup_emit(&self, state: &PopupState) {
        popup::emit_state(&self.app, state);
    }
    fn popup_show(&self) {
        popup::show(&self.app);
    }
    fn popup_hide(&self) {
        popup::hide(&self.app);
    }
    fn popup_clickable(&self, clickable: bool) {
        popup::set_clickable(&self.app, clickable);
    }
    fn history_changed(&self) {
        let _ = self.app.emit(EVENT_HISTORY_CHANGED, ());
    }
    fn hotkey(&self, cmd: HotkeyCmd) {
        let mut guard = self.hotkey.lock();
        match cmd {
            HotkeyCmd::Reinstall => {
                if let Some(h) = guard.as_ref() {
                    h.reinstall();
                } else {
                    match HotkeyService::start(self.hotkey_tx.clone()) {
                        Ok(h) => {
                            tracing::info!("hotkey installed after reinstall request");
                            *guard = Some(h);
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "still cannot install hotkey");
                        }
                    }
                }
            }
            HotkeyCmd::ForceIdle => {
                if let Some(h) = guard.as_ref() {
                    h.force_idle();
                }
            }
            HotkeyCmd::SetPaused(paused) => {
                if let Some(h) = guard.as_ref() {
                    h.set_paused(paused);
                }
            }
        }
    }
    fn play(&self, sound: Sound) {
        play_tick(&self.app, sound);
    }
    fn dictation_started(&self) {
        self.watcher.stop();
        if let Some(c) = &self.prepaste {
            c.prime();
        }
        // Load the clean-up model while he speaks, so the AI step fits its time limit.
        let ai = self.settings().ai_cleanup;
        if ai.enabled && !self.ai_warming.swap(true, Ordering::SeqCst) {
            let warming = self.ai_warming.clone();
            let spawned = std::thread::Builder::new().name("wren-ai-warm".into()).spawn(move || {
                ai_cleanup::warm(&ai.model);
                warming.store(false, Ordering::SeqCst);
            });
            if spawned.is_err() {
                self.ai_warming.store(false, Ordering::SeqCst);
            }
        }
    }
    fn pasted(&self, d: &PastedDictation) {
        let enabled = watcher::effective_enabled(self.settings().watcher_enabled);
        let Some(session) = self.session.get().cloned() else { return };
        let app = self.app.clone();
        self.watcher.start(d.clone(), enabled, move |d, end| {
            let store = &app.state::<AppState>().store;
            let learned = watcher::learn_from(store, &d.id, &end);
            if learned.is_empty() {
                return;
            }
            tracing::info!(
                id = %d.id,
                corrections = learned.len(),
                decisions = ?learned.iter().map(|l| &l.decision).collect::<Vec<_>>(),
                entry_ids = ?learned.iter().map(|l| l.entry_id).collect::<Vec<_>>(),
                "learned from a correction"
            );
            let _ = app.emit(EVENT_HISTORY_CHANGED, ());
            if let Some(toast) = watcher::toast_for(&learned) {
                session.popup_result(toast);
            }
        });
    }
    fn target_not_editable(&self, target: &ForegroundInfo) -> bool {
        self.prepaste
            .as_ref()
            .is_some_and(|c| c.check(target.pid, &target.process_name) == prepaste::PrePaste::NotEditable)
    }
    fn ai_cleanup(&self, tidied: &str) -> Option<String> {
        ai_cleanup::clean(&self.settings().ai_cleanup.model, tidied)
    }
}

/// Everything started at launch, kept in Tauri's managed state.
pub struct Services {
    pub env: Arc<AppEnv>,
    pub session: SessionHandle,
}

impl Services {
    /// Quit: let the session save a long recording and finish (or save) queued dictations — at most
    /// `QUIT_DRAIN_MAX_MS` — then unhook, stop the sidecar and put back any clipboard still owed.
    pub fn shutdown(&self) {
        let drained = self.session.shutdown(Duration::from_millis(QUIT_DRAIN_MAX_MS));
        if !drained {
            tracing::warn!(pending = self.session.pending(), "quit: dictations still unfinished after the wait");
        }
        self.env.shutdown();
    }
}

/// Start the recorder, engine, hotkey and session. Call from `setup`, after `AppState` is managed
/// and the windows exist (main thread).
pub fn start_services(app: &AppHandle) -> anyhow::Result<Services> {
    let state = app.state::<AppState>();
    let settings = state.settings.read().clone();

    // Microphone, with its level meter going to the popup.
    let (level_tx, level_rx) = crossbeam_channel::unbounded::<f32>();
    let recorder = match AudioRecorder::new(settings.mic_device.clone(), level_tx) {
        Ok(r) => Some(Arc::new(r)),
        Err(err) => {
            tracing::error!(error = %err, "cannot set up the microphone");
            None
        }
    };
    {
        let app = app.clone();
        std::thread::Builder::new().name("wren-levels".into()).spawn(move || {
            for level in level_rx {
                popup::emit_level(&app, level);
            }
        })?;
    }

    // Speech engine, its status forwarded to the UI.
    let (status_tx, status_rx) = crossbeam_channel::unbounded::<EngineStatus>();
    let engine = Engine::start(&state.paths, &settings.engine, status_tx.clone());
    {
        let app = app.clone();
        std::thread::Builder::new().name("wren-engine-status".into()).spawn(move || {
            for status in status_rx {
                match &status {
                    EngineStatus::Ready { model, device, vram_mb } => {
                        tracing::info!(ready = true, %model, %device, vram_mb, "engine status")
                    }
                    other => tracing::info!(ready = false, status = ?other, "engine status"),
                }
                let _ = app.emit(EVENT_ENGINE_STATUS, &status);
            }
        })?;
    }

    // Hotkey (its events go straight to the session).
    let (hotkey_tx, hotkey_rx) = crossbeam_channel::unbounded();
    let hotkey = match HotkeyService::start(hotkey_tx.clone()) {
        Ok(h) => Some(h),
        Err(err) => {
            #[cfg(target_os = "macos")]
            tracing::error!(error = %err, "cannot install the Fn+Control hotkey");
            #[cfg(not(target_os = "macos"))]
            tracing::error!(error = %err, "cannot install the Ctrl+Win hotkey");
            None
        }
    };
    let hotkey_failed = hotkey.is_none();

    #[cfg(windows)]
    let own_windows: Vec<isize> = app
        .webview_windows()
        .values()
        .filter_map(|w| w.hwnd().ok().map(|h| h.0 as isize))
        .collect();
    #[cfg(target_os = "macos")]
    let ax_watch_stop = Arc::new(AtomicBool::new(false));
    let env = Arc::new(AppEnv {
        app: app.clone(),
        recorder,
        engine: RwLock::new(Arc::new(engine)),
        status_tx,
        engine_swap: Mutex::new(()),
        swapping: AtomicBool::new(false),
        hotkey: Mutex::new(hotkey),
        hotkey_tx,
        #[cfg(windows)]
        own_windows,
        watcher: Watcher::new(),
        prepaste: prepaste::Checker::start().map_err(|e| tracing::warn!(error = %e, "no pre-paste check")).ok(),
        session: OnceLock::new(),
        ai_warming: Arc::new(AtomicBool::new(false)),
        #[cfg(target_os = "macos")]
        ax_watch_stop: ax_watch_stop.clone(),
    });
    let session = super::start(env.clone(), hotkey_rx)?;
    let _ = env.session.set(session.clone());

    // On macOS, if Accessibility was not granted yet at launch, poll gently in the background so
    // enabling Wren in System Settings activates `Fn + Control` automatically even while hidden.
    #[cfg(target_os = "macos")]
    if hotkey_failed {
        let env_watch = env.clone();
        let session_watch = session.clone();
        let stop = ax_watch_stop;
        let _ = std::thread::Builder::new().name("wren-ax-watch".into()).spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(2));
                if stop.load(Ordering::SeqCst) || env_watch.has_hotkey() {
                    break;
                }
                if crate::hotkey::accessibility_trusted(false) {
                    tracing::info!("macOS Accessibility granted after launch; installing Fn + Control hotkey");
                    session_watch.send(Msg::ReinstallHook);
                    // Wait briefly for the session to apply ReinstallHook before re-checking.
                    std::thread::sleep(Duration::from_millis(500));
                    if env_watch.has_hotkey() {
                        break;
                    }
                }
            }
        });
    }

    // Tray: pause and reinstall.
    {
        let session = session.clone();
        app.listen(tray::EVENT_PAUSE_TOGGLED, move |event| {
            let paused = serde_json::from_str::<bool>(event.payload()).unwrap_or(false);
            session.send(Msg::SetPaused(paused));
        });
    }
    {
        let session = session.clone();
        app.listen(tray::EVENT_REINSTALL_HOOK, move |_| session.send(Msg::ReinstallHook));
    }

    if hotkey_failed || env.recorder.is_none() {
        // Say so once the popup page has loaded.
        let session = session.clone();
        let message = if hotkey_failed {
            #[cfg(target_os = "macos")]
            {
                "Wren needs Accessibility permission for Fn + Control. Open Wren to allow it."
            }
            #[cfg(not(target_os = "macos"))]
            {
                "Wren couldn't set up Ctrl + Win. Restart Wren to try again."
            }
        } else {
            "Wren can't use your microphone."
        };
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(STARTUP_ERROR_POPUP_DELAY_MS));
            session.popup_result(PopupState::Error { message: message.into() });
        });
    }
    tracing::info!(hotkey = !hotkey_failed, microphone = env.recorder.is_some(), "services started");
    Ok(Services { env, session })
}

// ---- optional start/stop sounds (off by default) ----

/// A soft sine tick as a complete 16-bit mono WAV file (Hann envelope, so no click at the ends).
pub fn tick_wav(freq_hz: f32) -> Vec<u8> {
    let rate = ENGINE_SAMPLE_RATE;
    let n = (rate as u64 * SOUND_TICK_MS / 1000) as usize;
    let mut pcm = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / rate as f32;
        let env = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (n - 1).max(1) as f32).cos();
        let s = SOUND_TICK_AMPLITUDE * env * (2.0 * std::f32::consts::PI * freq_hz * t).sin();
        pcm.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
    }
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&rate.to_le_bytes());
    wav.extend_from_slice(&(rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}

fn play_tick(app: &AppHandle, sound: Sound) {
    static START: OnceLock<Vec<u8>> = OnceLock::new();
    static STOP: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes: &'static [u8] = match sound {
        Sound::Start => START.get_or_init(|| tick_wav(SOUND_START_HZ)),
        Sound::Stop => STOP.get_or_init(|| tick_wav(SOUND_STOP_HZ)),
    };
    #[cfg(windows)]
    {
        let _ = app;
        use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
        // SND_MEMORY reads the WAV from this (static, so always alive) buffer; SND_ASYNC returns at once.
        unsafe {
            let _ = PlaySoundW(
                windows::core::PCWSTR(bytes.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
            );
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Dispatch AppKit `NSSound` allocation, playback, stop, and release strictly on the main thread.
        let _ = app.run_on_main_thread(move || {
            objc2::rc::autoreleasepool(|_| {
                use objc2::AnyThread;
                use objc2_app_kit::NSSound;
                use objc2_foundation::NSData;
                static ACTIVE: Mutex<Option<usize>> = Mutex::new(None);
                let data = NSData::with_bytes(bytes);
                if let Some(snd) = NSSound::initWithData(NSSound::alloc(), &data) {
                    let _ = snd.play();
                    let raw = objc2::rc::Retained::into_raw(snd) as usize;
                    let prev = ACTIVE.lock().replace(raw);
                    if let Some(old) = prev {
                        unsafe {
                            if let Some(old_snd) = objc2::rc::Retained::from_raw(old as *mut NSSound) {
                                let _ = old_snd.stop();
                            }
                        }
                    }
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_is_a_valid_short_quiet_wav() {
        let wav = tick_wav(SOUND_START_HZ);
        let reader = hound::WavReader::new(std::io::Cursor::new(wav)).expect("valid WAV");
        let spec = reader.spec();
        assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (1, ENGINE_SAMPLE_RATE, 16));
        let samples: Vec<i16> = reader.into_samples::<i16>().map(|s| s.unwrap()).collect();
        assert_eq!(samples.len() as u64, ENGINE_SAMPLE_RATE as u64 * SOUND_TICK_MS / 1000);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak as f32 <= SOUND_TICK_AMPLITUDE * i16::MAX as f32 + 1.0, "soft: {peak}");
        assert!(peak > 0);
        assert!(samples[0].unsigned_abs() < 50 && samples.last().unwrap().unsigned_abs() < 50, "no click at the ends");
    }
}
