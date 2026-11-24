//! The session (spec §1, §3–§7): the sole owner of dictation state, dictation IDs and ordering.
//!
//! Two threads:
//! - **session** (`wren-session`): hears hotkey events and popup/tray requests, starts and stops the
//!   recorder, checks the 10 min cap, the hands-free silence stop and "No sound from mic", and is the
//!   only owner of the popup state (results from the finisher and Task 9 arrive as
//!   `Msg::PopupResult`; no locks). It never waits on the engine, a paste or the UI thread, so a new
//!   chord press is always answered at once.
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

pub mod app;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use serde::Serialize;

use crate::audio::Recording;
use crate::constants::{
    BIAS_TERMS_MAX, CANCEL_SAVE_AFTER_MS, HANDSFREE_SILENCE_STOP_MS, KEYSTATE_POLL_MS, KEYUP_POLL_MS,
    MAX_RECORDING_MS, NO_SOUND_WARN_MS, POPUP_DELAY_MS, POPUP_ERROR_MS, POPUP_HIDE_AFTER_DISMISS_MS,
    POPUP_LEARNED_MS, POPUP_NOTHING_MS, POPUP_NOT_PASTED_MS, POPUP_NO_SOUND_MS, POPUP_PASTED_MS,
    QUIT_DRAIN_MAX_MS, QUIT_ENGINE_WAIT_MS, RECORDING_WARN_MS, SIDECAR_POLL_MS, SIDECAR_READY_TIMEOUT_MS,
    TRANSCRIBE_TIMEOUT_MS, TRANSCRIBE_TIMEOUT_PER_CLIP, WARMUP_PASTE_GRACE_MS,
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
    /// `recording`: shown while still recording (a silent mic), so it stays until the recording
    /// replaces it; after a recording it dismisses itself like any notice (M-1).
    NoSound { recording: bool },
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
            Self::NoSound { recording } => write!(f, "NoSound {{ recording: {recording} }}"),
        }
    }
}

impl PopupState {
    /// How long the popup shows this state before dismissing itself (None = until replaced).
    pub fn dismiss_ms(&self) -> Option<u64> {
        match self {
            Self::Pasted => Some(POPUP_PASTED_MS),
            Self::Nothing => Some(POPUP_NOTHING_MS),
            Self::NoSound { recording: false } => Some(POPUP_NO_SOUND_MS),
            Self::NotPasted { .. } => Some(POPUP_NOT_PASTED_MS),
            Self::Learned { .. } => Some(POPUP_LEARNED_MS),
            Self::Error { .. } => Some(POPUP_ERROR_MS),
            Self::Hidden
            | Self::NoSound { recording: true }
            | Self::Listening { .. }
            | Self::HandsFree { .. }
            | Self::Warming
            | Self::Transcribing { .. }
            | Self::CapWarning { .. } => None,
        }
    }

    /// States that offer a click (copy, undo, open Wren, microphone settings, × discard). Everything
    /// else is click-through.
    pub fn is_clickable(&self) -> bool {
        matches!(
            self,
            Self::NotPasted { .. }
                | Self::NoSound { .. }
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
        PasteResult::FocusChanged | PasteResult::KeysStillHeld | PasteResult::OwnWindow | PasteResult::NotEditable => {
            (PasteOutcome::NotPasted, true)
        }
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

fn is_failed(s: &EngineStatus) -> bool {
    matches!(s, EngineStatus::Failed { .. })
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
    /// Ruling 24 (Task 9): true only when what has focus in `target` is definitely not editable, so
    /// the text is copied instead of pasted. Must answer within `PREPASTE_CHECK_MAX_MS`.
    fn target_not_editable(&self, _target: &ForegroundInfo) -> bool {
        false
    }
    /// The optional AI clean-up step (spec §10; Task 9). Only called when `aiCleanup.enabled` is on.
    /// Returns the cleaned text, or None (unreachable, slow, failed). `pipeline::run` checks the result.
    fn ai_cleanup(&self, _tidied: &str) -> Option<String> {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Session handle
// ---------------------------------------------------------------------------------------------

/// Requests to the session thread from commands, the tray, the finisher and Task 9's watcher.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// The popup's × (hands-free): discard the recording.
    Discard,
    SetPaused(bool),
    ReinstallHook,
    /// A finished dictation's (or the watcher's) popup state. Shown at once unless a recording is
    /// on screen; a notice (notPasted, error, learned) then waits until the recording ends.
    PopupResult(PopupState),
    /// Quit: a long recording is saved, queued dictations are finished or saved, then the threads end.
    Shutdown,
}

/// What the session thread and the finisher share. Atomics and one tiny lock, never held across a
/// call that can block.
struct Shared {
    /// A hold recording is in progress: its chord keys are down, so the finisher waits before pasting.
    hold_recording: AtomicBool,
    /// Dictations recorded but not yet finished.
    pending: AtomicUsize,
    /// Set when quitting: everything must be finished or saved by then.
    quit_at: parking_lot::Mutex<Option<Instant>>,
}

impl Shared {
    fn quit_at(&self) -> Option<Instant> {
        *self.quit_at.lock()
    }
}

/// Handle to the running session. Cheap to clone; every method returns at once except `shutdown`.
#[derive(Clone)]
pub struct SessionHandle {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    /// Disconnected once the finisher has ended (it holds the only sender).
    finished: Receiver<()>,
}

impl SessionHandle {
    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
    }

    /// Show a result state (e.g. Task 9's `learned` toast) unless a recording is on screen. Safe from
    /// any thread, including the main thread: it only posts a message (no lock, no window call).
    pub fn popup_result(&self, state: PopupState) {
        self.send(Msg::PopupResult(state));
    }

    /// Dictations recorded but not yet finished (transcribing or pasting).
    pub fn pending(&self) -> usize {
        self.shared.pending.load(Ordering::SeqCst)
    }

    /// Quit: stops the recording (saved if it ran ≥ `CANCEL_SAVE_AFTER_MS`), lets queued dictations
    /// finish or be saved untranscribed, and waits at most `max` for that. True if all were handled.
    pub fn shutdown(&self, max: Duration) -> bool {
        let deadline = Instant::now() + max;
        {
            let mut q = self.shared.quit_at.lock();
            if q.is_none() {
                *q = Some(deadline);
            }
        }
        self.send(Msg::Shutdown);
        matches!(self.finished.recv_deadline(deadline), Err(crossbeam_channel::RecvTimeoutError::Disconnected))
    }
}

/// Starts the session and finisher threads. `hotkey_rx` is the hotkey's (unbounded) channel.
pub fn start(env: Arc<dyn Env>, hotkey_rx: Receiver<HotkeyEvent>) -> anyhow::Result<SessionHandle> {
    let (tx, rx) = crossbeam_channel::unbounded::<Msg>();
    let (jobs_tx, jobs_rx) = crossbeam_channel::unbounded::<Job>();
    let (done_tx, done_rx) = crossbeam_channel::bounded::<()>(0);
    let shared = Arc::new(Shared {
        hold_recording: AtomicBool::new(false),
        pending: AtomicUsize::new(0),
        quit_at: parking_lot::Mutex::new(None),
    });

    {
        let (env, shared, post) = (env.clone(), shared.clone(), tx.clone());
        std::thread::Builder::new().name("wren-finisher".into()).spawn(move || {
            let _done = done_tx;
            let mut last_seq = 0;
            // Ends when the session thread drops its sender.
            for job in jobs_rx {
                debug_assert!(job.seq > last_seq, "jobs arrive in start order");
                last_seq = job.seq;
                finish_job(env.as_ref(), &shared, &post, job);
                shared.pending.fetch_sub(1, Ordering::SeqCst);
            }
        })?;
    }
    {
        let mut session = SessionLoop {
            popup: PopupCtl { env: env.clone(), current: PopupState::Hidden, hide_at: None, deferred: None },
            env,
            jobs: jobs_tx,
            shared: shared.clone(),
            active: None,
            seq: 0,
        };
        std::thread::Builder::new().name("wren-session".into()).spawn(move || session.run(hotkey_rx, rx))?;
    }
    Ok(SessionHandle { tx, shared, finished: done_rx })
}

// ---------------------------------------------------------------------------------------------
// Popup controller (session thread only: no locks)
// ---------------------------------------------------------------------------------------------

/// A state that must not be lost to a recording starting on top of it.
fn is_notice(s: &PopupState) -> bool {
    matches!(s, PopupState::NotPasted { .. } | PopupState::Error { .. } | PopupState::Learned { .. })
}

/// A state the current recording put up (so ending the recording may take it down).
fn is_recording_look(s: &PopupState) -> bool {
    matches!(
        s,
        PopupState::Listening { .. }
            | PopupState::HandsFree { .. }
            | PopupState::Warming
            | PopupState::NoSound { .. }
            | PopupState::CapWarning { .. }
    )
}

/// What the popup shows. Owned by the session thread, so changes are naturally ordered; the `Env`
/// popup calls it makes never block (see popup.rs).
struct PopupCtl {
    env: Arc<dyn Env>,
    current: PopupState,
    /// The session hides a transient state at this time (`dismiss_ms` + `POPUP_HIDE_AFTER_DISMISS_MS`).
    hide_at: Option<Instant>,
    /// A notice that arrived during a recording, or that a recording covered: shown when it ends.
    deferred: Option<PopupState>,
}

impl PopupCtl {
    /// Show `state` now (or hide, for `Hidden`). Transient states get a hide time unless a recording
    /// is running (they then stay until replaced).
    fn set(&mut self, state: PopupState, recording: bool) {
        self.hide_at = None;
        if state == PopupState::Hidden {
            if self.current != PopupState::Hidden {
                self.env.popup_emit(&state);
                self.env.popup_hide();
                self.env.popup_clickable(false);
            }
            self.current = PopupState::Hidden;
            return;
        }
        // Content first, then the window, so it never shows a stale state.
        self.env.popup_clickable(state.is_clickable());
        self.env.popup_emit(&state);
        self.env.popup_show();
        if !recording && let Some(dismiss) = state.dismiss_ms() {
            self.hide_at = Some(Instant::now() + Duration::from_millis(dismiss + POPUP_HIDE_AFTER_DISMISS_MS));
        }
        self.current = state;
    }

    /// Keep `notice` to show when the recording ends. One slot: a newer notice replaces the waiting
    /// one unless the waiting one matters more (M-2) — "Not pasted · click to copy" (he must act)
    /// over an error over a "Learned" toast.
    fn defer(&mut self, notice: PopupState) {
        let rank = |s: &PopupState| match s {
            PopupState::NotPasted { .. } => 3,
            PopupState::Error { .. } => 2,
            _ => 1,
        };
        if self.deferred.as_ref().is_none_or(|waiting| rank(&notice) >= rank(waiting)) {
            self.deferred = Some(notice);
        }
    }

    /// A recording's own look. A notice still on screen is kept to be shown again when it ends.
    fn set_recording(&mut self, state: PopupState) {
        if is_notice(&self.current) && self.hide_at.is_some() {
            self.defer(self.current.clone());
        }
        self.set(state, true);
    }

    /// The recording ended without a new state of its own: show a waiting notice, or take down
    /// the recording's look. Anything else on screen (an earlier dictation's notice) stays.
    fn recording_ended(&mut self) {
        if let Some(notice) = self.deferred.take() {
            self.set(notice, false);
        } else if is_recording_look(&self.current) {
            self.set(PopupState::Hidden, false);
        }
    }

    /// The recording ended and goes on to `next` (Transcribing…), unless a notice is waiting: that
    /// is shown instead (the next result replaces it).
    fn recording_ended_with(&mut self, next: PopupState) {
        match self.deferred.take() {
            Some(notice) => self.set(notice, false),
            None => self.set(next, false),
        }
    }

    fn tick(&mut self, now: Instant) {
        if let Some(at) = self.hide_at
            && now >= at
        {
            self.hide_at = None;
            self.current = PopupState::Hidden;
            self.env.popup_hide();
            self.env.popup_clickable(false);
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
    /// A long hold cancelled by a Windows shortcut or lock/sleep, or a long recording at quit: save
    /// to History, no popup, no paste.
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
    /// "No sound from mic" is up; back to the recording look once sound arrives.
    no_sound_shown: bool,
    /// The engine was not ready when the chord went down.
    warming: bool,
    cpu: bool,
    /// Epoch ms of the start (the hands-free timer counts from this).
    started_epoch_ms: i64,
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
    /// How long finishing the recording (WAV) took after the release.
    stop_ms: u64,
    target: ForegroundInfo,
    intent: Intent,
}

struct SessionLoop {
    env: Arc<dyn Env>,
    popup: PopupCtl,
    jobs: Sender<Job>,
    shared: Arc<Shared>,
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
                    Ok(Msg::Shutdown) | Err(_) => {
                        self.quit();
                        break;
                    }
                    Ok(msg) => self.on_msg(msg),
                },
                default(timeout) => {}
            }
            self.tick();
        }
        tracing::info!("session stopped");
        // Dropping `self.jobs` (with the loop) lets the finisher drain the queue and end.
    }

    /// Quit: a recording that ran long enough is saved like a cancelled long hold; a short one is
    /// dropped.
    fn quit(&mut self) {
        {
            let mut q = self.shared.quit_at.lock();
            if q.is_none() {
                *q = Some(Instant::now() + Duration::from_millis(QUIT_DRAIN_MAX_MS));
            }
        }
        if self.active.is_none() {
            return;
        }
        let elapsed = self.env.recorder().map(|r| r.elapsed_ms()).unwrap_or(0);
        if elapsed >= CANCEL_SAVE_AFTER_MS {
            tracing::info!(elapsed, "quitting during a recording; saving it");
            self.stop(Intent::SaveQuiet);
        } else {
            self.discard("quit");
        }
    }

    /// How long to sleep: until the next popup/no-sound/hide deadline, at most `KEYSTATE_POLL_MS`
    /// while recording (cap and silence checks), else a second.
    fn next_wakeup(&self) -> Duration {
        let now = Instant::now();
        let mut wake = now + Duration::from_millis(if self.active.is_some() { KEYSTATE_POLL_MS } else { 1_000 });
        if let Some(a) = &self.active {
            for due in [a.popup_due, a.no_sound_due].into_iter().flatten() {
                wake = wake.min(due);
            }
        }
        if let Some(at) = self.popup.hide_at {
            wake = wake.min(at);
        }
        wake.saturating_duration_since(now)
    }

    fn on_hotkey(&mut self, ev: HotkeyEvent) {
        match ev {
            HotkeyEvent::HoldStart => self.begin(Mode::Hold),
            HotkeyEvent::HandsFreeStart => self.begin(Mode::HandsFree),
            HotkeyEvent::Tap => self.discard("tap"),
            HotkeyEvent::HoldEnd { .. } | HotkeyEvent::HandsFreeStop => self.stop(Intent::Paste),
            HotkeyEvent::Cancel { held_ms } => {
                if held_ms >= CANCEL_SAVE_AFTER_MS {
                    self.stop(Intent::SaveQuiet);
                } else {
                    self.discard("cancel");
                }
            }
        }
    }

    fn on_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Discard => {
                if self.active.is_some() {
                    self.discard("popup_discard");
                    self.env.hotkey(HotkeyCmd::ForceIdle);
                }
            }
            Msg::SetPaused(paused) => {
                tracing::info!(paused, "dictation paused from the tray");
                if paused {
                    self.discard("paused");
                }
                self.env.hotkey(HotkeyCmd::SetPaused(paused));
            }
            Msg::ReinstallHook => {
                tracing::info!("reinstalling the hotkey (tray)");
                self.env.hotkey(HotkeyCmd::Reinstall);
            }
            Msg::PopupResult(state) => {
                if self.active.is_none() {
                    self.popup.set(state, false);
                } else if is_notice(&state) {
                    // Not lost: shown when this recording ends.
                    self.popup.defer(state);
                }
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
        let env = self.env.clone();
        let Some(rec) = env.recorder() else {
            self.popup.set(PopupState::Error { message: "Wren can't use your microphone.".into() }, false);
            env.hotkey(HotkeyCmd::ForceIdle);
            return;
        };
        let id = uuid::Uuid::new_v4().to_string();
        let wav = env.audio_dir().join(format!("{id}.wav"));
        let started = Instant::now();
        let created_at = chrono::Utc::now();
        // The chord keys are down from now, not from when the mic opens (tens of ms; over a second
        // on a cold or Bluetooth mic): an earlier dictation must not paste meanwhile (I-1).
        self.shared.hold_recording.store(mode == Mode::Hold, Ordering::SeqCst);
        // The microphone first: nothing else may delay the first word.
        if let Err(err) = rec.start(wav) {
            self.shared.hold_recording.store(false, Ordering::SeqCst);
            tracing::error!(error = %err, "cannot start recording");
            self.popup.set(PopupState::Error { message: "Wren couldn't open your microphone.".into() }, false);
            env.hotkey(HotkeyCmd::ForceIdle);
            return;
        }
        env.dictation_started();
        let status = env.engine_status();
        let (warming, cpu) = (!is_ready(&status), is_cpu(&status));
        let started_epoch_ms = created_at.timestamp_millis();
        let popup_due = match mode {
            Mode::Hold => Some(started + Duration::from_millis(POPUP_DELAY_MS)),
            Mode::HandsFree => {
                self.popup.set_recording(PopupState::HandsFree { started_at_ms: started_epoch_ms, cpu });
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
            no_sound_shown: false,
            warming,
            cpu,
            started_epoch_ms,
        });
    }

    /// Timed checks: the popup's hide time, and while recording the popup delay, "No sound", the cap
    /// and the hands-free silence stop.
    fn tick(&mut self) {
        let now = Instant::now();
        self.popup.tick(now);
        let env = self.env.clone();
        let Some(rec) = env.recorder() else { return };
        let Some(a) = self.active.as_mut() else { return };
        if let Some(due) = a.popup_due
            && now >= due
        {
            a.popup_due = None;
            let status = env.engine_status();
            let state = if is_failed(&status) {
                PopupState::Error { message: ENGINE_STOPPED_MESSAGE.into() }
            } else if a.warming && !is_ready(&status) {
                PopupState::Warming
            } else {
                PopupState::Listening { cpu: a.cpu }
            };
            self.popup.set_recording(state);
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
                a.no_sound_shown = true;
                self.popup.set_recording(PopupState::NoSound { recording: true });
            }
        } else if a.no_sound_shown {
            let elapsed = rec.elapsed_ms();
            if rec.silence_ms() < elapsed {
                a.no_sound_shown = false;
                self.popup.set_recording(match a.mode {
                    Mode::Hold => PopupState::Listening { cpu: a.cpu },
                    Mode::HandsFree => PopupState::HandsFree { started_at_ms: a.started_epoch_ms, cpu: a.cpu },
                });
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
            self.popup.set_recording(PopupState::CapWarning { remaining_ms: MAX_RECORDING_MS.saturating_sub(elapsed), mode });
        }
        if a.mode == Mode::HandsFree && rec.silence_ms() >= HANDSFREE_SILENCE_STOP_MS {
            tracing::info!(id = %a.id, "hands-free stopped after long silence");
            env.hotkey(HotkeyCmd::ForceIdle);
            self.stop(Intent::SaveNotice);
        }
    }

    /// Drop the recording and its audio. Takes down only this recording's popup: an earlier
    /// dictation's notice still on screen stays (a tap or Ctrl+Win+→ must not hide it).
    fn discard(&mut self, reason: &'static str) {
        let Some(a) = self.active.take() else { return };
        let t = Instant::now();
        if let Some(rec) = self.env.recorder() {
            rec.cancel();
        }
        let stop_ms = ms(t.elapsed());
        self.shared.hold_recording.store(false, Ordering::SeqCst);
        self.popup.recording_ended();
        tracing::info!(id = %a.id, stop_ms, reason, "recording discarded");
    }

    /// Stop the recording and hand it to the finisher.
    fn stop(&mut self, intent: Intent) {
        let Some(a) = self.active.take() else { return };
        let env = self.env.clone();
        let Some(rec) = env.recorder() else { return };
        // Spec §7: the target is the window in front when the dictation stops — read before stopping
        // (finishing the WAV takes a moment, and focus can move meanwhile).
        let target = env.foreground();
        let released = Instant::now();
        let stopped = rec.stop();
        let stop_ms = ms(released.elapsed());
        self.shared.hold_recording.store(false, Ordering::SeqCst);
        if intent == Intent::Paste {
            self.sound(Sound::Stop);
        }
        let recording = match stopped {
            Ok(r) => r,
            Err(err) => {
                tracing::error!(id = %a.id, error = %err, "cannot finish the recording");
                self.popup.recording_ended_with(PopupState::Error { message: "Wren couldn't finish the recording.".into() });
                return;
            }
        };
        if !recording.heard_sound {
            tracing::info!(id = %a.id, "nothing above the silence floor; not transcribed");
            let _ = std::fs::remove_file(&recording.wav_path);
            if intent == Intent::SaveQuiet {
                self.popup.recording_ended();
            } else {
                self.popup.recording_ended_with(PopupState::NoSound { recording: false });
            }
            return;
        }
        match intent {
            Intent::Paste | Intent::SaveNotice => {
                let status = env.engine_status();
                let next = if is_ready(&status) {
                    PopupState::Transcribing { since_ms: epoch_ms() }
                } else if is_failed(&status) {
                    PopupState::Error { message: ENGINE_STOPPED_MESSAGE.into() }
                } else {
                    PopupState::Warming
                };
                self.popup.recording_ended_with(next);
            }
            Intent::SaveQuiet => self.popup.recording_ended(),
        }
        self.seq += 1;
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let job = Job {
            seq: self.seq,
            id: a.id,
            mode: a.mode,
            created_at: a.created_at,
            recording,
            released,
            record_ms: ms(released.duration_since(a.started)),
            stop_ms,
            target,
            intent,
        };
        if self.jobs.send(job).is_err() {
            self.shared.pending.fetch_sub(1, Ordering::SeqCst);
            tracing::error!("the finisher has stopped; dictation dropped");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Finisher: engine → pipeline → store → paste, one dictation at a time in start order
// ---------------------------------------------------------------------------------------------

/// The optional AI clean-up step as `pipeline::run` takes it.
type AiStep<'a> = &'a dyn Fn(&str) -> Option<String>;

/// The popup message when a dictation could not be transcribed (its audio is kept in History).
pub const NOT_TRANSCRIBED_MESSAGE: &str = "Couldn't transcribe — it's saved in History.";
/// Shown instead of "Warming up…" when the engine has given up (M-3); clicking opens Wren.
pub const ENGINE_STOPPED_MESSAGE: &str = "Speech engine stopped — open Wren.";

fn drop_audio(job: &Job) {
    let _ = std::fs::remove_file(&job.recording.wav_path);
}

enum EngineReady {
    Yes { model: String, device: String },
    No(String),
}

/// Waits up to `limit` for the engine to be ready. `waited` is set if it was not ready at once.
fn wait_ready(env: &dyn Env, limit: Duration, waited: &mut bool) -> EngineReady {
    let t = Instant::now();
    loop {
        match env.engine_status() {
            EngineStatus::Ready { model, device, .. } => return EngineReady::Yes { model, device },
            EngineStatus::Failed { reason } => return EngineReady::No(reason),
            EngineStatus::Starting | EngineStatus::WarmingUp => {
                *waited = true;
                if t.elapsed() >= limit {
                    return EngineReady::No("the speech engine did not get ready in time".into());
                }
                std::thread::sleep(Duration::from_millis(SIDECAR_POLL_MS));
            }
        }
    }
}

/// How long to wait for the engine: normally `SIDECAR_READY_TIMEOUT_MS`; when quitting at most
/// `QUIT_ENGINE_WAIT_MS` and never past the quit deadline.
fn engine_wait_limit(shared: &Shared) -> Duration {
    match shared.quit_at() {
        Some(q) => Duration::from_millis(QUIT_ENGINE_WAIT_MS).min(q.saturating_duration_since(Instant::now())),
        None => Duration::from_millis(SIDECAR_READY_TIMEOUT_MS),
    }
}

/// Transcription time limit, scaled with the clip (the CPU fallback is slow on long clips) and
/// bounded by the quit deadline.
fn transcribe_timeout(shared: &Shared, clip_ms: u64) -> Duration {
    let normal = Duration::from_millis(TRANSCRIBE_TIMEOUT_MS + (clip_ms as f64 * TRANSCRIBE_TIMEOUT_PER_CLIP) as u64);
    match shared.quit_at() {
        Some(q) => normal.min(q.saturating_duration_since(Instant::now())).max(Duration::from_secs(1)),
        None => normal,
    }
}

/// Saves a dictation that could not be transcribed: no text, audio kept, outcome `failed`.
fn save_untranscribed(env: &dyn Env, job: &Job, model: &str, device: &str, why: &str) {
    let row = NewTranscription {
        id: job.id.clone(),
        created_at_utc: job.created_at,
        mode: job.mode,
        audio_path: Some(job.recording.wav_path.clone()),
        duration_ms: job.recording.duration_ms,
        speech_ms: 0,
        raw_text: String::new(),
        tidied_text: String::new(),
        final_text: String::new(),
        model: model.into(),
        device: device.into(),
        timings_json: serde_json::json!({ "recordMs": job.record_ms, "transcribed": false }).to_string(),
        target_process: job.target.process_name.clone(),
        paste_outcome: PasteOutcome::Failed,
        applied_rule_ids: Vec::new(),
    };
    match env.store().insert_transcription(&row) {
        Ok(()) => {
            tracing::warn!(id = %job.id, why, "saved without text (not transcribed); audio kept");
            env.history_changed();
        }
        Err(err) => tracing::error!(id = %job.id, error = %err, "cannot save the untranscribed dictation"),
    }
}

fn finish_job(env: &dyn Env, shared: &Shared, post: &Sender<Msg>, job: Job) {
    let quiet = job.intent == Intent::SaveQuiet;
    let popup = |state: PopupState| {
        let _ = post.send(Msg::PopupResult(state));
    };
    let show = |state: PopupState| {
        if !quiet {
            popup(state);
        }
    };
    if shared.quit_at().is_some_and(|q| Instant::now() >= q) {
        save_untranscribed(env, &job, "", "", "quit before it could be transcribed");
        return;
    }

    // 1. The engine must be ready (pressed during warm-up: wait for it).
    let wait_start = Instant::now();
    let mut warm = false;
    let mut ready = wait_ready(env, engine_wait_limit(shared), &mut warm);
    let wait_ready_ms = ms(wait_start.elapsed());
    if warm && matches!(ready, EngineReady::Yes { .. }) {
        show(PopupState::Transcribing { since_ms: epoch_ms() });
    }

    // 2. Transcribe with the ranked dictionary terms (the sidecar builds the Glossary prompt).
    //    On an engine error, retry once when the engine is ready again; then save without text.
    let settings = env.settings();
    let store = env.store();
    let bias_terms = store.bias_terms(BIAS_TERMS_MAX).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "cannot read bias terms");
        Vec::new()
    });
    let prompt = glossary_prompt(&bias_terms);
    let request = TranscribeRequest {
        id: job.id.clone(),
        audio_path: job.recording.wav_path.clone(),
        bias_terms,
        prompt: String::new(),
    };
    let (mut model, mut device) = (String::new(), String::new());
    let mut transcribe_ms = 0;
    let mut attempts = 0;
    let result = loop {
        attempts += 1;
        let error = match ready {
            EngineReady::Yes { model: m, device: d } => {
                (model, device) = (m, d);
                let t = Instant::now();
                let r = env.transcribe(request.clone(), transcribe_timeout(shared, job.recording.duration_ms));
                transcribe_ms += ms(t.elapsed());
                match r {
                    Ok(r) => break Some(r),
                    Err(err) => err.to_string(),
                }
            }
            EngineReady::No(reason) => reason,
        };
        tracing::warn!(id = %job.id, attempt = attempts, error = %error, "transcription failed");
        if attempts >= 2 || shared.quit_at().is_some() {
            break None;
        }
        let mut waited = false;
        ready = wait_ready(env, engine_wait_limit(shared), &mut waited);
    };
    let Some(result) = result else {
        save_untranscribed(env, &job, &model, &device, "the engine failed");
        show(PopupState::Error { message: NOT_TRANSCRIBED_MESSAGE.into() });
        return;
    };

    // 3. Text pipeline (spec §6), with the AI step only when it is switched on.
    let replacements = store.active_replacements().unwrap_or_else(|err| {
        tracing::warn!(error = %err, "cannot read replacements");
        Vec::new()
    });
    let t = Instant::now();
    let ai_step = |tidied: &str| env.ai_cleanup(tidied);
    let ai: Option<AiStep> = if settings.ai_cleanup.enabled { Some(&ai_step) } else { None };
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
    let timings = serde_json::json!({
        "recordMs": job.record_ms,
        "waitReadyMs": wait_ready_ms,
        "transcribeMs": transcribe_ms,
        "attempts": attempts,
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
    let (mut paste_ms, mut release_to_paste_ms, mut held_wait_ms, mut late) = (None, None, 0, false);
    if job.intent == Intent::Paste {
        // A hold recording that started after this one has its chord keys down: wait for it to end
        // (it queues behind this dictation, so start order is kept), then paste waits for key-up.
        let t = Instant::now();
        while shared.hold_recording.load(Ordering::SeqCst) && shared.quit_at().is_none() {
            std::thread::sleep(Duration::from_millis(KEYUP_POLL_MS));
        }
        held_wait_ms = ms(t.elapsed());
        // Spec §5: after a warm-up, paste only within the grace period of the release — measured
        // to the moment of pasting.
        late = warm && job.released.elapsed() > Duration::from_millis(WARMUP_PASTE_GRACE_MS);
    }
    if job.intent == Intent::Paste && !late {
        let t = Instant::now();
        let r = if env.target_not_editable(&job.target) { PasteResult::NotEditable } else { env.paste(&text, &job.target) };
        paste_ms = Some(ms(t.elapsed()));
        release_to_paste_ms = Some(ms(job.released.elapsed()));
        let (o, must_copy) = outcome_for(&r);
        outcome = o;
        if must_copy && let Err(err) = env.copy_text(&text) {
            tracing::warn!(id = %job.id, error = %err, "cannot copy the unpasted text");
        }
        if r == PasteResult::Pasted {
            popup(PopupState::Pasted);
            env.pasted(&PastedDictation { id: job.id.clone(), text: text.clone(), target: job.target.clone() });
        } else {
            tracing::info!(id = %job.id, result = ?r, "not pasted");
            popup(PopupState::NotPasted { text: text.clone() });
        }
        if saved {
            if let Err(err) = store.set_paste_outcome(&job.id, outcome) {
                tracing::warn!(id = %job.id, error = %err, "cannot record the paste outcome");
            }
            env.history_changed();
        }
    } else if !quiet {
        // Warm-up took too long, or hands-free stopped on silence: saved, offered to copy.
        popup(PopupState::NotPasted { text: text.clone() });
    }

    tracing::info!(
        id = %job.id,
        seq = job.seq,
        mode = ?job.mode,
        intent = ?job.intent,
        outcome = ?outcome,
        warm,
        late,
        attempts,
        record_ms = job.record_ms,
        stop_ms = job.stop_ms,
        wait_ready_ms,
        transcribe_ms,
        infer_ms = result.infer_ms,
        speech_ms = result.speech_ms,
        pipeline_ms,
        held_wait_ms,
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
