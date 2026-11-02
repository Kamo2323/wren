//! The session (spec §1, §3–§7): the sole owner of dictation state, dictation IDs and ordering.
//!
//! Two threads:
//! - **session** (`wren-session`): hears hotkey events and popup/tray requests, starts and stops the
//!   recorder, drives the popup while recording and checks the 10 min cap, the hands-free silence stop
//!   and "No sound from mic". It never waits on the engine or on a paste, so a new chord press is
//!   always answered at once.
//! - **finisher** (`wren-finisher`): takes finished recordings one at a time, in the order they were
//!   started, and runs engine → pipeline → store → paste → popup. One FIFO worker is what guarantees
//!   "dictations finishing out of order paste in start order" (spec §7): a second dictation can be
//!   recorded while the first is still transcribing, but it is pasted after it.
//!
//! Everything outside this file is reached through [`Env`], so the whole flow is unit-tested with a
//! fake recorder, engine, paste and popup (tests at the bottom). [`AppEnv`] is the real one.
//!
//! Privacy: dictated text only ever goes to the store, the clipboard/paste and the popup. Logs carry
//! ids, timings, counts and outcomes. `PopupState`'s `Debug` is redacted.
//!
//! Hooks for Task 9: [`Env::dictation_started`] (stop the correction watcher), [`Env::pasted`] (start
//! it after a successful paste) and [`Env::ai_cleanup`] (the optional AI step handed to
//! `pipeline::run`).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use serde::Serialize;

use crate::audio::Recording;
use crate::constants::{
    BIAS_TERMS_MAX, CANCEL_SAVE_AFTER_MS, HANDSFREE_SILENCE_STOP_MS, KEYSTATE_POLL_MS, MAX_RECORDING_MS,
    NO_SOUND_WARN_MS, POPUP_DELAY_MS, POPUP_ERROR_MS, POPUP_HIDE_AFTER_DISMISS_MS, POPUP_LEARNED_MS,
    POPUP_NOTHING_MS, POPUP_NOT_PASTED_MS, POPUP_NO_SOUND_MS, POPUP_PASTED_MS, RECORDING_WARN_MS,
    SIDECAR_POLL_MS, SIDECAR_READY_TIMEOUT_MS, TRANSCRIBE_TIMEOUT_MS, WARMUP_PASTE_GRACE_MS,
};
use crate::dictionary::{EntryKind, Status};
use crate::engine::{EngineError, EngineStatus, TranscribeRequest, TranscribeResult};
use crate::hotkey::HotkeyEvent;
use crate::paste::{ForegroundInfo, PasteResult, REASON_NOT_READ};
use crate::pipeline::{self, PipelineInput};
use crate::settings::Settings;
use crate::store::{Mode, NewTranscription, PasteOutcome, Store};

// ---------------------------------------------------------------------------------------------
// Popup state (contract with src/lib/types.ts `PopupState`)
// ---------------------------------------------------------------------------------------------

/// Which kind of recording a cap warning belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CapMode {
    Hold,
    HandsFree,
}

/// What the popup shows. Serialises exactly as the TypeScript `PopupState` union. All timestamps
/// are epoch milliseconds (`Date.now()` scale).
#[derive(Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum PopupState {
    Hidden,
    Listening { cpu: bool },
    #[serde(rename = "handsfree")]
    HandsFree { started_at_ms: i64, cpu: bool },
    Warming,
    Transcribing { since_ms: i64 },
    Pasted,
    Nothing,
    NoSound,
    NotPasted { text: String },
    Learned { from: String, to: String, entry_id: i64 },
    Error { message: String },
    CapWarning { remaining_ms: u64, mode: CapMode },
}

impl std::fmt::Debug for PopupState {
    /// Redacted: never prints dictated or learned text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotPasted { text } => write!(f, "NotPasted {{ text_chars: {} }}", text.chars().count()),
            Self::Learned { entry_id, .. } => write!(f, "Learned {{ entry_id: {entry_id} }}"),
            Self::Error { message } => write!(f, "Error {{ message: {message:?} }}"),
            Self::Listening { cpu } => write!(f, "Listening {{ cpu: {cpu} }}"),
            Self::HandsFree { started_at_ms, cpu } => write!(f, "HandsFree {{ started_at_ms: {started_at_ms}, cpu: {cpu} }}"),
            Self::Transcribing { since_ms } => write!(f, "Transcribing {{ since_ms: {since_ms} }}"),
            Self::CapWarning { remaining_ms, mode } => write!(f, "CapWarning {{ remaining_ms: {remaining_ms}, mode: {mode:?} }}"),
            Self::Hidden => f.write_str("Hidden"),
            Self::Warming => f.write_str("Warming"),
            Self::Pasted => f.write_str("Pasted"),
            Self::Nothing => f.write_str("Nothing"),
            Self::NoSound => f.write_str("NoSound"),
        }
    }
}

impl PopupState {
    /// How long the popup shows this state before dismissing itself (None = until replaced).
    pub fn dismiss_ms(&self) -> Option<u64> {
        match self {
            Self::Pasted => Some(POPUP_PASTED_MS),
            Self::Nothing => Some(POPUP_NOTHING_MS),
            Self::NoSound => Some(POPUP_NO_SOUND_MS),
            Self::NotPasted { .. } => Some(POPUP_NOT_PASTED_MS),
            Self::Learned { .. } => Some(POPUP_LEARNED_MS),
            Self::Error { .. } => Some(POPUP_ERROR_MS),
            Self::Hidden
            | Self::Listening { .. }
            | Self::HandsFree { .. }
            | Self::Warming
            | Self::Transcribing { .. }
            | Self::CapWarning { .. } => None,
        }
    }

    /// States that offer a click (copy, undo, open Wren, × discard). Everything else is click-through.
    pub fn is_clickable(&self) -> bool {
        matches!(
            self,
            Self::NotPasted { .. }
                | Self::Learned { .. }
                | Self::Error { .. }
                | Self::HandsFree { .. }
                | Self::CapWarning { mode: CapMode::HandsFree, .. }
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------------------------

/// What the store records for a paste result, and whether the caller must put the text on the
/// clipboard itself (nothing was sent to the app, spec §7 "Not pasted — click to copy").
pub fn outcome_for(result: &PasteResult) -> (PasteOutcome, bool) {
    match result {
        PasteResult::Pasted => (PasteOutcome::Pasted, false),
        PasteResult::FocusChanged | PasteResult::KeysStillHeld | PasteResult::OwnWindow => (PasteOutcome::NotPasted, true),
        // Ctrl+V and Ctrl+Shift+V went out and nobody read: paste left the text on the clipboard.
        PasteResult::Failed(reason) if reason == REASON_NOT_READ => (PasteOutcome::Copied, false),
        PasteResult::Failed(_) => (PasteOutcome::Failed, true),
    }
}

/// Ids of active dictionary terms that appear in `text` (case-insensitive, whole word). Every id
/// at most once, in the order given.
pub fn term_hits(text: &str, terms: &[(i64, String)]) -> Vec<i64> {
    let hay = text.to_lowercase();
    let mut out = Vec::new();
    for (id, term) in terms {
        let needle = term.trim().to_lowercase();
        if needle.is_empty() || out.contains(id) {
            continue;
        }
        if contains_whole_word(&hay, &needle) {
            out.push(*id);
        }
    }
    out
}

/// `needle` occurs in `hay` with no letter or digit touching either end.
fn contains_whole_word(hay: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = hay[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = hay[..start].chars().next_back().is_none_or(|c| !c.is_alphanumeric());
        let after_ok = hay[end..].chars().next().is_none_or(|c| !c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + hay[start..].chars().next().map(char::len_utf8).unwrap_or(1);
    }
    false
}

/// The prompt the sidecar builds from the bias terms (spec §5), for the pipeline's prompt-echo
/// guard. The sidecar may drop terms to fit its token budget; a superset only makes the guard
/// slightly more careful.
fn glossary_prompt(bias_terms: &[String]) -> String {
    if bias_terms.is_empty() { String::new() } else { format!("Glossary: {}.", bias_terms.join(", ")) }
}

/// Epoch milliseconds now.
fn epoch_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn is_ready(s: &EngineStatus) -> bool {
    matches!(s, EngineStatus::Ready { .. })
}

fn is_cpu(s: &EngineStatus) -> bool {
    matches!(s, EngineStatus::Ready { device, .. } if device == "cpu")
}

fn ms(d: Duration) -> u64 {
    d.as_millis() as u64
}

// ---------------------------------------------------------------------------------------------
// The outside world
// ---------------------------------------------------------------------------------------------

/// The recorder as the session uses it (`audio::AudioRecorder` in the app, a fake in tests).
pub trait Recorder: Send + Sync {
    fn start(&self, wav_path: PathBuf) -> anyhow::Result<()>;
    fn stop(&self) -> anyhow::Result<Recording>;
    fn cancel(&self);
    fn silence_ms(&self) -> u64;
    fn elapsed_ms(&self) -> u64;
}

impl Recorder for crate::audio::AudioRecorder {
    fn start(&self, wav_path: PathBuf) -> anyhow::Result<()> {
        crate::audio::AudioRecorder::start(self, wav_path)
    }
    fn stop(&self) -> anyhow::Result<Recording> {
        crate::audio::AudioRecorder::stop(self)
    }
    fn cancel(&self) {
        crate::audio::AudioRecorder::cancel(self)
    }
    fn silence_ms(&self) -> u64 {
        crate::audio::AudioRecorder::silence_ms(self)
    }
    fn elapsed_ms(&self) -> u64 {
        crate::audio::AudioRecorder::elapsed_ms(self)
    }
}

/// Requests the session makes of the hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyCmd {
    /// The session stopped a recording on its own (cap, silence, ×): the chord machine forgets it.
    ForceIdle,
    SetPaused(bool),
    Reinstall,
}

/// Optional start/stop ticks (`sounds` setting).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sound {
    Start,
    Stop,
}

/// What a successful paste hands to the correction watcher (Task 9).
#[derive(Clone)]
pub struct PastedDictation {
    pub id: String,
    pub text: String,
    pub target: ForegroundInfo,
}

/// Everything the session needs from outside. `AppEnv` is the real implementation.
pub trait Env: Send + Sync + 'static {
    fn store(&self) -> &Store;
    fn settings(&self) -> Settings;
    fn audio_dir(&self) -> PathBuf;
    fn recorder(&self) -> Option<&dyn Recorder>;
    fn engine_status(&self) -> EngineStatus;
    fn transcribe(&self, req: TranscribeRequest, timeout: Duration) -> Result<TranscribeResult, EngineError>;
    fn foreground(&self) -> ForegroundInfo;
    fn paste(&self, text: &str, target: &ForegroundInfo) -> PasteResult;
    fn copy_text(&self, text: &str) -> anyhow::Result<()>;
    /// Send the state to the popup page.
    fn popup_emit(&self, state: &PopupState);
    /// Show the popup window without activating it.
    fn popup_show(&self);
    fn popup_hide(&self);
    fn popup_clickable(&self, clickable: bool);
    /// `app://history-changed`.
    fn history_changed(&self);
    fn hotkey(&self, cmd: HotkeyCmd);
    fn play(&self, sound: Sound);
    /// A new recording started (Task 9: stop any running correction watcher).
    fn dictation_started(&self) {}
    /// Text was pasted and read by the target (Task 9: start the correction watcher here).
    fn pasted(&self, _dictation: &PastedDictation) {}
    /// The optional AI clean-up step (spec §10; Task 9). Only called when `aiCleanup.enabled` is on.
    /// Returns the cleaned text, or None (unreachable, slow, failed). `pipeline::run` checks the result.
    fn ai_cleanup(&self, _tidied: &str) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Session handle
// ---------------------------------------------------------------------------------------------

/// Requests to the session thread from commands and the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// The popup's × (hands-free): discard the recording.
    Discard,
    SetPaused(bool),
    ReinstallHook,
    Shutdown,
}

/// Handle to the running session. Cheap to clone.
#[derive(Clone)]
pub struct SessionHandle {
    tx: Sender<Msg>,
    popup: Arc<PopupCtl>,
    pending: Arc<AtomicUsize>,
}

impl SessionHandle {
    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    /// Show a result state (e.g. Task 9's `learned` toast) unless a recording is on screen.
    pub fn popup_result(&self, state: PopupState) {
        self.popup.result(state);
    }

    /// Dictations recorded but not yet finished (transcribing or pasting).
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }
}

/// Starts the session and finisher threads. `hotkey_rx` is the hotkey's (unbounded) channel.
pub fn start(env: Arc<dyn Env>, hotkey_rx: Receiver<HotkeyEvent>) -> anyhow::Result<SessionHandle> {
    let (tx, rx) = crossbeam_channel::unbounded::<Msg>();
    let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
    let popup = Arc::new(PopupCtl::new(env.clone()));
    let pending = Arc::new(AtomicUsize::new(0));

    {
        let (env, popup, pending) = (env.clone(), popup.clone(), pending.clone());
        std::thread::Builder::new().name("wren-finisher".into()).spawn(move || {
            let mut last_seq = 0;
            // Ends when the session thread drops its sender.
            for job in jobs_rx {
                debug_assert!(job.seq > last_seq, "jobs arrive in start order");
                last_seq = job.seq;
                finish_job(env.as_ref(), &popup, job);
                pending.fetch_sub(1, Ordering::SeqCst);
            }
        })?;
    }
    {
        let mut session =
            SessionLoop { env, popup: popup.clone(), jobs: jobs_tx, pending: pending.clone(), active: None, seq: 0 };
        std::thread::Builder::new().name("wren-session".into()).spawn(move || session.run(hotkey_rx, rx))?;
    }
    Ok(SessionHandle { tx, popup, pending })
}

// ---------------------------------------------------------------------------------------------
// Popup controller
// ---------------------------------------------------------------------------------------------

struct PopupInner {
    /// Bumped by every change, so a pending "hide after the dismiss time" from an older state
    /// never hides a newer one.
    generation: u64,
    /// A recording is on screen: finished dictations do not replace it with their result.
    recording: bool,
    /// Something is (or is about to be) visible.
    shown: bool,
}

/// Owns what the popup shows. All changes go through one lock, shared with the hide timers.
pub struct PopupCtl {
    env: Arc<dyn Env>,
    inner: Arc<parking_lot::Mutex<PopupInner>>,
}

impl PopupCtl {
    fn new(env: Arc<dyn Env>) -> Self {
        Self { env, inner: Arc::new(parking_lot::Mutex::new(PopupInner { generation: 0, recording: false, shown: false })) }
    }

    fn begin_recording(&self) {
        self.inner.lock().recording = true;
    }

    fn end_recording(&self) {
        self.inner.lock().recording = false;
    }

    fn is_shown(&self) -> bool {
        self.inner.lock().shown
    }

    /// Show `state` now (or hide, for `Hidden`). Transient states are hidden by the session
    /// `POPUP_HIDE_AFTER_DISMISS_MS` after the popup's own dismiss time — except during a recording.
    fn set(&self, state: PopupState) {
        let mut g = self.inner.lock();
        self.set_locked(&mut g, state);
    }

    fn set_locked(&self, g: &mut PopupInner, state: PopupState) {
        g.generation += 1;
        let generation = g.generation;
        if state == PopupState::Hidden {
            g.shown = false;
            self.env.popup_emit(&state);
            self.env.popup_hide();
            self.env.popup_clickable(false);
            return;
        }
        g.shown = true;
        // Content first, then the window, so it never shows a stale state.
        self.env.popup_clickable(state.is_clickable());
        self.env.popup_emit(&state);
        self.env.popup_show();
        if let Some(dismiss) = state.dismiss_ms()
            && !g.recording
        {
            let (env, inner) = (self.env.clone(), self.inner.clone());
            let spawned = std::thread::Builder::new().name("wren-popup-hide".into()).spawn(move || {
                std::thread::sleep(Duration::from_millis(dismiss + POPUP_HIDE_AFTER_DISMISS_MS));
                let mut g = inner.lock();
                if g.generation == generation {
                    g.shown = false;
                    env.popup_hide();
                    env.popup_clickable(false);
                }
            });
            if let Err(err) = spawned {
                tracing::warn!(error = %err, "cannot start the popup hide timer");
            }
        }
    }

    /// A finished dictation's state: shown unless a new recording is on screen. Checked and set
    /// under one lock, so a recording starting at the same moment always wins.
    fn result(&self, state: PopupState) {
        let mut g = self.inner.lock();
        if !g.recording {
            self.set_locked(&mut g, state);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Session thread
// ---------------------------------------------------------------------------------------------

/// What happens to a stopped recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    /// Transcribe and paste (chord released, hands-free stopped, cap reached).
    Paste,
    /// A long hold cancelled by a Windows shortcut or lock/sleep: save to History, no popup, no paste.
    SaveQuiet,
    /// Hands-free stopped by silence: save, don't paste, show "Not pasted — click to copy".
    SaveNotice,
}

/// The recording in progress.
struct Active {
    id: String,
    mode: Mode,
    started: Instant,
    created_at: chrono::DateTime<chrono::Utc>,
    /// Hold: show Listening / Warming up at this time (so Ctrl+Win+→ never flashes the popup).
    popup_due: Option<Instant>,
    /// Check for "No sound from mic" at this time.
    no_sound_due: Option<Instant>,
    warned: bool,
    /// The engine was not ready when the chord went down.
    warming: bool,
    cpu: bool,
}

/// A stopped recording handed to the finisher.
struct Job {
    seq: u64,
    id: String,
    mode: Mode,
    created_at: chrono::DateTime<chrono::Utc>,
    recording: Recording,
    released: Instant,
    record_ms: u64,
    target: ForegroundInfo,
    intent: Intent,
}

struct SessionLoop {
    env: Arc<dyn Env>,
    popup: Arc<PopupCtl>,
    jobs: Sender<Job>,
    pending: Arc<AtomicUsize>,
    active: Option<Active>,
    seq: u64,
}

impl SessionLoop {
    fn run(&mut self, hotkey_rx: Receiver<HotkeyEvent>, rx: Receiver<Msg>) {
        let mut hotkey_rx = hotkey_rx;
        loop {
            let timeout = self.next_wakeup();
            crossbeam_channel::select! {
                recv(hotkey_rx) -> ev => match ev {
                    Ok(ev) => self.on_hotkey(ev),
                    Err(_) => {
                        tracing::warn!("hotkey channel closed");
                        hotkey_rx = crossbeam_channel::never();
                    }
                },
                recv(rx) -> msg => match msg {
                    Ok(Msg::Shutdown) | Err(_) => break,
                    Ok(msg) => self.on_msg(msg),
                },
                default(timeout) => {}
            }
            self.tick();
        }
        if self.active.take().is_some()
            && let Some(rec) = self.env.recorder()
        {
            rec.cancel();
        }
        tracing::info!("session stopped");
    }

    /// How long to sleep: until the next popup/no-sound deadline, at most `KEYSTATE_POLL_MS` while
    /// recording (cap and silence checks), else a second.
    fn next_wakeup(&self) -> Duration {
        let Some(a) = &self.active else { return Duration::from_secs(1) };
        let now = Instant::now();
        let mut wake = now + Duration::from_millis(KEYSTATE_POLL_MS);
        for due in [a.popup_due, a.no_sound_due].into_iter().flatten() {
            wake = wake.min(due);
        }
        wake.saturating_duration_since(now)
    }

    fn on_hotkey(&mut self, ev: HotkeyEvent) {
        match ev {
            HotkeyEvent::HoldStart => self.begin(Mode::Hold),
            HotkeyEvent::HandsFreeStart => self.begin(Mode::HandsFree),
            HotkeyEvent::Tap => self.discard(),
            HotkeyEvent::HoldEnd { .. } | HotkeyEvent::HandsFreeStop => self.stop(Intent::Paste),
            HotkeyEvent::Cancel { held_ms } => {
                if held_ms >= CANCEL_SAVE_AFTER_MS {
                    self.stop(Intent::SaveQuiet);
                } else {
                    self.discard();
                }
            }
        }
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Discard => {
                if self.active.is_some() {
                    self.discard();
                    self.env.hotkey(HotkeyCmd::ForceIdle);
                }
            }
            Msg::SetPaused(paused) => {
                tracing::info!(paused, "dictation paused from the tray");
                if paused {
                    self.discard();
                }
                self.env.hotkey(HotkeyCmd::SetPaused(paused));
            }
            Msg::ReinstallHook => {
                tracing::info!("reinstalling the hotkey (tray)");
                self.env.hotkey(HotkeyCmd::Reinstall);
            }
            Msg::Shutdown => {}
        }
    }

    fn sound(&self, sound: Sound) {
        if self.env.settings().sounds {
            self.env.play(sound);
        }
    }

    fn begin(&mut self, mode: Mode) {
        if self.active.is_some() {
            tracing::warn!(?mode, "chord pressed while already recording; ignored");
            return;
        }
        let Some(rec) = self.env.recorder() else {
            self.popup.set(PopupState::Error { message: "Wren can't use your microphone.".into() });
            self.env.hotkey(HotkeyCmd::ForceIdle);
            return;
        };
        self.env.dictation_started();
        let id = uuid::Uuid::new_v4().to_string();
        let wav = self.env.audio_dir().join(format!("{id}.wav"));
        let started = Instant::now();
        let created_at = chrono::Utc::now();
        self.popup.begin_recording();
        if let Err(err) = rec.start(wav) {
            tracing::error!(error = %err, "cannot start recording");
            self.popup.end_recording();
            self.popup.set(PopupState::Error { message: "Wren couldn't open your microphone.".into() });
            self.env.hotkey(HotkeyCmd::ForceIdle);
            return;
        }
        let status = self.env.engine_status();
        let (warming, cpu) = (!is_ready(&status), is_cpu(&status));
        let popup_due = match mode {
            Mode::Hold => Some(started + Duration::from_millis(POPUP_DELAY_MS)),
            Mode::HandsFree => {
                self.popup.set(PopupState::HandsFree { started_at_ms: epoch_ms(), cpu });
                self.sound(Sound::Start);
                None
            }
        };
        tracing::info!(%id, ?mode, warming, "recording started");
        self.active = Some(Active {
            id,
            mode,
            started,
            created_at,
            popup_due,
            no_sound_due: Some(started + Duration::from_millis(NO_SOUND_WARN_MS)),
            warned: false,
            warming,
            cpu,
        });
    }

    /// Timed checks while recording.
    fn tick(&mut self) {
        let env = self.env.clone();
        let Some(rec) = env.recorder() else { return };
        let Some(a) = self.active.as_mut() else { return };
        let now = Instant::now();
        if let Some(due) = a.popup_due
            && now >= due
        {
            a.popup_due = None;
            let state = if a.warming && !is_ready(&env.engine_status()) {
                PopupState::Warming
            } else {
                PopupState::Listening { cpu: a.cpu }
            };
            self.popup.set(state);
            if env.settings().sounds {
                env.play(Sound::Start);
            }
        }
        if let Some(due) = a.no_sound_due
            && now >= due
        {
            a.no_sound_due = None;
            // Elapsed first: if nothing was ever heard, silence (read second) is at least as long.
            let elapsed = rec.elapsed_ms();
            if rec.silence_ms() >= elapsed {
                tracing::info!(id = %a.id, "no sound from the microphone");
                self.popup.set(PopupState::NoSound);
            }
        }
        let elapsed = rec.elapsed_ms();
        if elapsed >= MAX_RECORDING_MS {
            tracing::info!(id = %a.id, "recording cap reached; stopping");
            env.hotkey(HotkeyCmd::ForceIdle);
            self.stop(Intent::Paste);
            return;
        }
        if elapsed >= RECORDING_WARN_MS && !a.warned {
            a.warned = true;
            let mode = if a.mode == Mode::HandsFree { CapMode::HandsFree } else { CapMode::Hold };
            self.popup.set(PopupState::CapWarning { remaining_ms: MAX_RECORDING_MS.saturating_sub(elapsed), mode });
        }
        if a.mode == Mode::HandsFree && rec.silence_ms() >= HANDSFREE_SILENCE_STOP_MS {
            tracing::info!(id = %a.id, "hands-free stopped after long silence");
            env.hotkey(HotkeyCmd::ForceIdle);
            self.stop(Intent::SaveNotice);
        }
    }

    /// Drop the recording and its audio; hide the popup if it was up.
    fn discard(&mut self) {
        let Some(a) = self.active.take() else { return };
        if let Some(rec) = self.env.recorder() {
            rec.cancel();
        }
        self.popup.end_recording();
        if self.popup.is_shown() {
            self.popup.set(PopupState::Hidden);
        }
        tracing::info!(id = %a.id, "recording discarded");
    }

    /// Stop the recording and hand it to the finisher.
    fn stop(&mut self, intent: Intent) {
        let Some(a) = self.active.take() else { return };
        let env = self.env.clone();
        let Some(rec) = env.recorder() else { return };
        let released = Instant::now();
        let stopped = rec.stop();
        self.popup.end_recording();
        if intent == Intent::Paste {
            self.sound(Sound::Stop);
        }
        let recording = match stopped {
            Ok(r) => r,
            Err(err) => {
                tracing::error!(id = %a.id, error = %err, "cannot finish the recording");
                self.popup.set(PopupState::Error { message: "Wren couldn't finish the recording.".into() });
                return;
            }
        };
        if !recording.heard_sound {
            tracing::info!(id = %a.id, "nothing above the silence floor; not transcribed");
            let _ = std::fs::remove_file(&recording.wav_path);
            if intent == Intent::SaveQuiet {
                if self.popup.is_shown() {
                    self.popup.set(PopupState::Hidden);
                }
            } else {
                self.popup.set(PopupState::NoSound);
            }
            return;
        }
        // Spec §7: the target is the foreground window when the dictation stops.
        let target = env.foreground();
        match intent {
            Intent::Paste | Intent::SaveNotice => {
                let state = if is_ready(&env.engine_status()) {
                    PopupState::Transcribing { since_ms: epoch_ms() }
                } else {
                    PopupState::Warming
                };
                self.popup.set(state);
            }
            Intent::SaveQuiet => {
                if self.popup.is_shown() {
                    self.popup.set(PopupState::Hidden);
                }
            }
        }
        self.seq += 1;
        self.pending.fetch_add(1, Ordering::SeqCst);
        let job = Job {
            seq: self.seq,
            id: a.id,
            mode: a.mode,
            created_at: a.created_at,
            recording,
            released,
            record_ms: ms(released.duration_since(a.started)),
            target,
            intent,
        };
        if self.jobs.send(job).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            tracing::error!("the finisher has stopped; dictation dropped");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Finisher: engine → pipeline → store → paste, one dictation at a time in start order
// ---------------------------------------------------------------------------------------------

fn drop_audio(job: &Job) {
    let _ = std::fs::remove_file(&job.recording.wav_path);
}

fn finish_job(env: &dyn Env, popup: &PopupCtl, job: Job) {
    let quiet = job.intent == Intent::SaveQuiet;
    let show = |state: PopupState| {
        if !quiet {
            popup.result(state);
        }
    };

    // 1. The engine must be ready (pressed during warm-up: wait for it).
    let wait_start = Instant::now();
    let mut warm = false;
    let (model, device) = loop {
        match env.engine_status() {
            EngineStatus::Ready { model, device, .. } => break (model, device),
            EngineStatus::Failed { reason } => {
                tracing::warn!(id = %job.id, "engine failed; dictation dropped");
                show(PopupState::Error { message: reason });
                drop_audio(&job);
                return;
            }
            EngineStatus::Starting | EngineStatus::WarmingUp => {
                warm = true;
                if wait_start.elapsed() >= Duration::from_millis(SIDECAR_READY_TIMEOUT_MS) {
                    tracing::warn!(id = %job.id, "engine not ready in time; dictation dropped");
                    show(PopupState::Error { message: "The speech engine is still starting. Try again in a moment.".into() });
                    drop_audio(&job);
                    return;
                }
                std::thread::sleep(Duration::from_millis(SIDECAR_POLL_MS));
            }
        }
    };
    let wait_ready_ms = ms(wait_start.elapsed());
    let ready_after_release_ms = ms(job.released.elapsed());
    if warm {
        show(PopupState::Transcribing { since_ms: epoch_ms() });
    }

    // 2. Transcribe with the ranked dictionary terms (the sidecar builds the Glossary prompt).
    let settings = env.settings();
    let store = env.store();
    let bias_terms = store.bias_terms(BIAS_TERMS_MAX).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "cannot read bias terms");
        Vec::new()
    });
    let prompt = glossary_prompt(&bias_terms);
    let t = Instant::now();
    let result = env.transcribe(
        TranscribeRequest {
            id: job.id.clone(),
            audio_path: job.recording.wav_path.clone(),
            bias_terms,
            prompt: String::new(),
        },
        Duration::from_millis(TRANSCRIBE_TIMEOUT_MS),
    );
    let transcribe_ms = ms(t.elapsed());
    let result = match result {
        Ok(r) => r,
        Err(err) => {
            tracing::warn!(id = %job.id, error = %err, "transcription failed");
            show(PopupState::Error { message: err.to_string() });
            drop_audio(&job);
            return;
        }
    };

    // 3. Text pipeline (spec §6), with the AI step only when it is switched on.
    let replacements = store.active_replacements().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "cannot read replacements");
        Vec::new()
    });
    let t = Instant::now();
    let ai_step = |tidied: &str| env.ai_cleanup(tidied);
    let ai: Option<&dyn Fn(&str) -> Option<String>> = if settings.ai_cleanup.enabled { Some(&ai_step) } else { None };
    let out = pipeline::run(
        &PipelineInput {
            raw: &result.text,
            speech_ms: result.speech_ms,
            clip_ms: job.recording.duration_ms,
            prompt: &prompt,
            replacements: &replacements,
        },
        ai,
    );
    let pipeline_ms = ms(t.elapsed());
    if out.final_text.is_empty() {
        tracing::info!(id = %job.id, speech_ms = result.speech_ms, transcribe_ms, "nothing to paste");
        show(PopupState::Nothing);
        drop_audio(&job);
        return;
    }
    let text = out.final_text.clone();

    // 4. Save BEFORE pasting (spec §7), so History always has it.
    let late = warm && ready_after_release_ms > WARMUP_PASTE_GRACE_MS;
    let paste_now = job.intent == Intent::Paste && !late;
    let timings = serde_json::json!({
        "recordMs": job.record_ms,
        "waitReadyMs": wait_ready_ms,
        "transcribeMs": transcribe_ms,
        "inferMs": result.infer_ms,
        "pipelineMs": pipeline_ms,
        "warm": warm,
    });
    let row = NewTranscription {
        id: job.id.clone(),
        created_at_utc: job.created_at,
        mode: job.mode,
        audio_path: Some(job.recording.wav_path.clone()),
        duration_ms: job.recording.duration_ms,
        speech_ms: result.speech_ms,
        raw_text: result.text.clone(),
        tidied_text: out.tidied.clone(),
        final_text: text.clone(),
        model,
        device,
        timings_json: timings.to_string(),
        target_process: job.target.process_name.clone(),
        paste_outcome: PasteOutcome::NotPasted,
        applied_rule_ids: out.applied_rule_ids.clone(),
    };
    let saved = match store.insert_transcription(&row) {
        Ok(()) => true,
        Err(err) => {
            tracing::error!(id = %job.id, error = %err, "cannot save the dictation; pasting anyway");
            false
        }
    };
    if saved {
        record_hits(store, &text, &out.applied_rule_ids);
        env.history_changed();
    }

    // 5. Paste (or not), then the popup.
    let mut outcome = PasteOutcome::NotPasted;
    let (mut paste_ms, mut release_to_paste_ms) = (None, None);
    if paste_now {
        let t = Instant::now();
        let r = env.paste(&text, &job.target);
        paste_ms = Some(ms(t.elapsed()));
        release_to_paste_ms = Some(ms(job.released.elapsed()));
        let (o, must_copy) = outcome_for(&r);
        outcome = o;
        if must_copy && let Err(err) = env.copy_text(&text) {
            tracing::warn!(id = %job.id, error = %err, "cannot copy the unpasted text");
        }
        if r == PasteResult::Pasted {
            popup.result(PopupState::Pasted);
            env.pasted(&PastedDictation { id: job.id.clone(), text: text.clone(), target: job.target.clone() });
        } else {
            tracing::info!(id = %job.id, result = ?r, "not pasted");
            popup.result(PopupState::NotPasted { text: text.clone() });
        }
        if saved {
            if let Err(err) = store.set_paste_outcome(&job.id, outcome) {
                tracing::warn!(id = %job.id, error = %err, "cannot record the paste outcome");
            }
            env.history_changed();
        }
    } else if !quiet {
        // Warm-up took too long, or hands-free stopped on silence: saved, offered to copy.
        popup.result(PopupState::NotPasted { text: text.clone() });
    }

    tracing::info!(
        id = %job.id,
        seq = job.seq,
        mode = ?job.mode,
        intent = ?job.intent,
        outcome = ?outcome,
        warm,
        late,
        record_ms = job.record_ms,
        wait_ready_ms,
        transcribe_ms,
        infer_ms = result.infer_ms,
        speech_ms = result.speech_ms,
        pipeline_ms,
        paste_ms,
        release_to_paste_ms,
        words = text.split_whitespace().count(),
        repetition_flagged = out.repetition_flagged,
        target = %job.target.process_name,
        "dictation done"
    );
}

/// Spec §8 / Task 4: hits for applied replacements and for active terms found in the final text.
fn record_hits(store: &Store, text: &str, applied: &[i64]) {
    let terms: Vec<(i64, String)> = match store.dictionary_list() {
        Ok(list) => list
            .into_iter()
            .filter(|e| e.kind == EntryKind::Term && e.status == Status::Active)
            .map(|e| (e.id, e.to_text))
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "cannot read dictionary terms");
            Vec::new()
        }
    };
    let mut ids = applied.to_vec();
    for id in term_hits(text, &terms) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    if let Err(err) = store.record_hits(&ids) {
        tracing::warn!(error = %err, "cannot record dictionary hits");
    }
}

#[cfg(test)]
mod tests;
