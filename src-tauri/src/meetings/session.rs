//! The meeting recorder (meetings spec §5.1–5.3, plan Task 7).
//!
//! Threads per meeting:
//! - **pipeline** (`wren-meeting`): owns both audio sources, writes `me.wav` / `them.wav`, runs one
//!   segmenter per stream, records Teams observations, and queues finished segments.
//! - **worker** (`wren-meeting-asr`): transcribes queued segments one at a time (so a dictation waits
//!   behind at most one meeting clip), names them, drops bleed-through and dictated speech, and saves.
//! - **Teams watcher** (optional): reads the Teams window every 250 ms.
//!
//! Stream positions are milliseconds since the meeting started (wall clock, pauses included), so
//! segment times, observations, dictation windows and the WAVs all share one timeline. While paused,
//! silence is written to the WAVs and nothing is transcribed.

use crate::meetings::capture::wav::ContinuousWav;
use crate::meetings::capture::{AudioBlock, AudioSource};
use crate::meetings::dedupe::{is_bleed, is_echo, starts_after, DictationGate, ECHO_MAX_LAG_MS};
use crate::meetings::diagnostics;
use crate::meetings::segmenter::{RawSegment, Segmenter, SegmenterConfig};
use crate::meetings::speakers::{SpeakerTimeline, TeamsObservation, TEAMS_LAG_MS, UNKNOWN_BEFORE_FIRST_MUTE_READ_MS};
use crate::meetings::teams::{spawn_watcher, ReaderFactory};
use crate::meetings::{CaptureMode, MeetingRow, MeetingStatus, NewSegment, SegmentRow, Source};
use crate::store::Store;
use crossbeam_channel::{select, Receiver, Sender};
use parking_lot::{Condvar, Mutex, RwLock};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Text saved for a segment that couldn't be transcribed in time (spec anti-goal: no silent gaps).
pub const NOT_TRANSCRIBED: &str = "[not transcribed]";
const SAMPLE_RATE: u32 = 16_000;
/// Above this much queued audio the header shows "Catching up…".
const CATCH_UP_MS: u64 = 60_000;
/// A Me clip waits (at most this long) until the Them stream has passed its end, so bleed-through
/// can be compared against what Them said at the same time.
const ME_WAIT_FOR_THEM: Duration = Duration::from_secs(2);
/// A Them clip waits (at most this long) for the Teams observations covering it.
const THEM_WAIT_FOR_TEAMS: Duration = Duration::from_millis(1_500);
/// Them lines kept in memory for the bleed check.
const RECENT_THEM_MS: u64 = 30_000;
const STATUS_EVERY: Duration = Duration::from_secs(1);
const TEAMS_POLL: Duration = Duration::from_millis(250);
/// Longest a Me clip waits for the Teams watcher to read past its end, so its mute state is known.
const ME_WAIT_FOR_TEAMS: Duration = Duration::from_secs(3);
/// Longest Stop waits for the Teams watcher to finish its current read and release the reader.
const WATCHER_JOIN: Duration = Duration::from_secs(2);
/// Longest quit waits for a meeting that is still opening its audio.
const QUIT_WAITS_FOR_START: Duration = Duration::from_secs(15);

/// Turns a clip into text (the dictation engine in the app; a fake in tests).
pub trait Transcriber: Send + Sync {
    fn transcribe(&self, wav: &Path, clip_ms: u64, bias: &[String]) -> Result<String, String>;
}

pub type SourcePair = (Box<dyn AudioSource>, Box<dyn AudioSource>);

pub struct Deps {
    pub store: Arc<Store>,
    pub transcriber: Arc<dyn Transcriber>,
    pub meetings_dir: PathBuf,
    /// `(mic, Teams)` sources for a new meeting.
    pub make_sources: Arc<dyn Fn() -> SourcePair + Send + Sync>,
    /// Builds the Teams window reader factory (None: no watcher, e.g. tests).
    pub make_teams_reader: Option<Arc<dyn Fn() -> ReaderFactory + Send + Sync>>,
    pub bias_terms: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    pub me_name: Arc<dyn Fn() -> String + Send + Sync>,
    /// "Save call diagnostics" (read when a meeting starts): write `call-diagnostics.jsonl`.
    pub save_diagnostics: Arc<dyn Fn() -> bool + Send + Sync>,
    /// How long Stop waits for queued clips before marking the rest "[not transcribed]".
    pub drain_timeout: Duration,
    /// Most audio kept waiting for transcription; older clips beyond it are saved as gaps (memory bound).
    pub max_backlog_ms: u64,
}



/// The running meeting, as the UI sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveMeeting {
    pub id: String,
    pub status: MeetingStatus,
    pub started_at_utc: String,
    pub elapsed_ms: u64,
    pub capture_mode: CaptureMode,
    pub catching_up: bool,
    pub speaking_me: bool,
    pub speaking_them: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum MeetingEvent {
    Segment { segment: SegmentRow },
    Speaking { meeting_id: String, source: Source, on: bool },
    /// `None` once the meeting has stopped.
    Status { active: Option<ActiveMeeting> },
    Participants { meeting_id: String, names: Vec<String> },
    Title { meeting_id: String, title: String },
}

pub type Listener = Arc<dyn Fn(MeetingEvent) + Send + Sync>;

#[derive(Debug, Clone)]
struct Job {
    seq: u64,
    source: Source,
    start_ms: u64,
    end_ms: u64,
    samples: Vec<f32>,
    queued_at: Instant,
}

#[derive(Default)]
struct Queue {
    next_seq: u64,
    jobs: VecDeque<Job>,
    in_flight: Option<(u64, Source, u64, u64)>,
    closed: bool,
}

#[derive(Default)]
struct Clock {
    paused_since: Option<Instant>,
    paused_total: Duration,
}

struct Shared {
    id: String,
    t0: Instant,
    started_at_utc: String,
    me_name: String,
    clock: Mutex<Clock>,
    capture_mode: Mutex<CaptureMode>,
    speaking: [AtomicBool; 2],
    queue: Mutex<Queue>,
    queue_cv: Condvar,
    timeline: Mutex<SpeakerTimeline>,
    has_observations: AtomicBool,
    gate: Mutex<DictationGate>,
    recent_them: Mutex<VecDeque<(u64, u64, String)>>,
    /// Recent Me lines, to drop Teams echoing them back (see `dedupe::is_echo`).
    recent_me: Mutex<VecDeque<(u64, u64, String)>>,
    /// Latest Them stream position the pipeline has processed, and when it last moved.
    them_pos: AtomicU64,
    them_seen: Mutex<Option<Instant>>,
    /// End of the Me audio seen so far, and where its open segment started (`u64::MAX`: none). A
    /// Them clip that may echo a Me line still being spoken waits for it (see `next_job`).
    me_pos: AtomicU64,
    me_open_since: AtomicU64,
    abort: AtomicBool,
    published: AtomicBool,
    listener: Arc<RwLock<Option<Listener>>>,
    dir: PathBuf,
    max_backlog_ms: u64,
    /// The Teams watcher thread (`None`: no reader) and its stop flag.
    watcher: Mutex<Option<JoinHandle<()>>>,
    watcher_stop: Arc<AtomicBool>,
    /// Call diagnostics file (`None`: the setting is off, or writing failed).
    diagnostics: Mutex<Option<diagnostics::Writer>>,
}

impl Shared {
    /// Run `f` on the diagnostics writer, if any; a write error turns diagnostics off for this meeting.
    fn diagnose(&self, f: impl FnOnce(&mut diagnostics::Writer) -> std::io::Result<()>) {
        let mut d = self.diagnostics.lock();
        if let Some(w) = d.as_mut()
            && let Err(e) = f(w)
        {
            tracing::warn!(error_kind = ?e.kind(), "could not write call diagnostics; stopped for this meeting");
            *d = None;
        }
    }

    /// True while the Teams watcher is still reading (a Me clip may wait for it).
    fn teams_watching(&self) -> bool {
        !self.watcher_stop.load(Ordering::Relaxed) && self.watcher.lock().as_ref().is_some_and(|h| !h.is_finished())
    }

    fn now_ms(&self) -> u64 {
        self.t0.elapsed().as_millis() as u64
    }

    fn elapsed_ms(&self) -> u64 {
        let c = self.clock.lock();
        let paused = c.paused_total + c.paused_since.map_or(Duration::ZERO, |p| p.elapsed());
        self.t0.elapsed().saturating_sub(paused).as_millis() as u64
    }

    fn paused(&self) -> bool {
        self.clock.lock().paused_since.is_some()
    }

    fn emit(&self, e: MeetingEvent) {
        if !self.published.load(Ordering::Acquire) {
            return;
        }
        let l = self.listener.read().clone();
        if let Some(l) = l {
            l(e);
        }
    }

    fn backlog_ms(&self) -> u64 {
        let q = self.queue.lock();
        q.jobs.iter().map(|j| j.end_ms - j.start_ms).sum::<u64>() + q.in_flight.map_or(0, |(_, _, s, e)| e - s)
    }

    fn snapshot(&self) -> ActiveMeeting {
        ActiveMeeting {
            id: self.id.clone(),
            status: if self.paused() { MeetingStatus::Paused } else { MeetingStatus::Recording },
            started_at_utc: self.started_at_utc.clone(),
            elapsed_ms: self.elapsed_ms(),
            capture_mode: *self.capture_mode.lock(),
            catching_up: self.backlog_ms() > CATCH_UP_MS,
            speaking_me: self.speaking[0].load(Ordering::Relaxed),
            speaking_them: self.speaking[1].load(Ordering::Relaxed),
        }
    }
}

enum Ctl {
    Pause(Sender<()>),
    Resume(Sender<()>),
    Stop,
}

struct Running {
    shared: Arc<Shared>,
    ctl: Sender<Ctl>,
    obs: Sender<TeamsObservation>,
    pipeline: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
    watcher_stop: Arc<AtomicBool>,
}

pub struct MeetingService {
    deps: Deps,
    running: Mutex<Option<Running>>,
    /// Set while `start` opens the audio (outside the `running` lock, so dictation never waits on it).
    starting: AtomicBool,
    /// The meeting `stop` is still draining (delete, start and quit must wait for it).
    stopping: Mutex<Option<String>>,
    stopped_cv: Condvar,
    /// Recent dictation windows (wall clock), kept even with no meeting running, so a meeting that is
    /// still starting when the hotkey goes down seeds its gate from them.
    dictation: Mutex<VecDeque<(Instant, Option<Instant>)>>,
    max_backlog_ms: AtomicU64,
    /// Set when Wren quits: a Stop still saving cuts its wait for queued clips short (to this deadline)
    /// and writes no notes; no new meeting starts.
    quitting: Mutex<Option<Instant>>,
    /// One meeting transcription at a time across this service's meetings: a stopping meeting's slow
    /// clip and the next meeting's first clip never both queue on the sidecar ahead of a dictation.
    asr_permit: Arc<Mutex<()>>,
    listener: Arc<RwLock<Option<Listener>>>,
    on_stopped: RwLock<Option<Arc<dyn Fn(String) + Send + Sync>>>,
}

fn idx(s: Source) -> usize {
    match s {
        Source::Me => 0,
        Source::Them => 1,
    }
}

fn default_title() -> String {
    chrono::Local::now().format("Meeting %-d %b, %H:%M").to_string()
}

fn write_clip(path: &Path, samples: &[f32]) -> anyhow::Result<()> {
    let spec = hound::WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()?;
    Ok(())
}

impl MeetingService {
    pub fn new(deps: Deps) -> Self {
        let max = deps.max_backlog_ms;
        Self {
            deps,
            running: Mutex::new(None),
            starting: AtomicBool::new(false),
            stopping: Mutex::new(None),
            stopped_cv: Condvar::new(),
            dictation: Mutex::new(VecDeque::new()),
            max_backlog_ms: AtomicU64::new(max),
            quitting: Mutex::new(None),
            asr_permit: Arc::new(Mutex::new(())),
            listener: Arc::new(RwLock::new(None)),
            on_stopped: RwLock::new(None),
        }
    }

    pub fn set_listener(&self, l: Listener) {
        *self.listener.write() = Some(l);
    }

    /// Called with the meeting id after a meeting stops (enhanced notes hook, plan Task 10).
    pub fn set_on_stopped(&self, f: Arc<dyn Fn(String) + Send + Sync>) {
        *self.on_stopped.write() = Some(f);
    }

    pub fn meeting_dir(&self, id: &str) -> PathBuf {
        self.deps.meetings_dir.join(id)
    }

    pub fn active(&self) -> Option<ActiveMeeting> {
        self.running.lock().as_ref().map(|r| r.shared.snapshot())
    }

    /// True while a meeting is starting, recording or still being saved (quit must ask / wait).
    pub fn in_progress(&self) -> bool {
        self.starting.load(Ordering::SeqCst) || self.running.lock().is_some() || self.stopping.lock().is_some()
    }

    /// True while meeting `id` records or is still being stopped (deleting it then would race).
    pub fn is_busy(&self, id: &str) -> bool {
        self.running.lock().as_ref().is_some_and(|r| r.shared.id == id) || self.stopping.lock().as_deref() == Some(id)
    }

    #[cfg(test)]
    pub(crate) fn set_max_backlog_ms(&mut self, ms: u64) {
        self.max_backlog_ms.store(ms, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn backlog_ms_for_test(&self) -> u64 {
        self.running.lock().as_ref().map_or(0, |r| r.shared.backlog_ms())
    }

    /// Start recording a new meeting. Fails if one is already running. Opening the audio happens
    /// outside the `running` lock: dictation's start/stop notices take that lock and must not wait.
    pub fn start(&self, title: Option<String>) -> Result<MeetingRow, String> {
        {
            let quitting = self.quitting.lock();
            if quitting.is_some() {
                return Err("Wren is quitting.".into());
            }
            // `stop` takes the meeting and marks it stopping in one step under `running`: check both there.
            let running = self.running.lock();
            if self.stopping.lock().is_some() {
                return Err("The last meeting is still being saved. Try again in a moment.".into());
            }
            if running.is_some() || self.starting.swap(true, Ordering::SeqCst) {
                return Err("A meeting is already being recorded.".into());
            }
        }
        struct Starting<'a>(&'a AtomicBool);
        impl Drop for Starting<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _starting = Starting(&self.starting);
        // One clock for everything: both sources count their stream positions from (about) now, and
        // observations and dictation windows are measured from the same instant.
        let t0 = Instant::now();
        let (mut mic, mut teams) = (self.deps.make_sources)();
        let (me_tx, me_rx) = crossbeam_channel::unbounded();
        let (them_tx, them_rx) = crossbeam_channel::unbounded();
        let (mode_tx, mode_rx) = crossbeam_channel::unbounded();
        let (dummy_tx, _dummy_rx) = crossbeam_channel::unbounded();
        if let Err(e) = mic.start(me_tx, dummy_tx) {
            tracing::warn!(error = %e, "meeting microphone failed to start");
        }
        let mode = match teams.start(them_tx, mode_tx) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "Teams audio capture failed to start");
                CaptureMode::MicOnly
            }
        };
        let title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let row = match self.deps.store.meeting_create(title.as_deref().unwrap_or(&default_title()), mode) {
            Ok(r) => r,
            Err(e) => {
                mic.stop();
                teams.stop();
                return Err(e.to_string());
            }
        };
        if title.is_some() {
            let _ = self.deps.store.meeting_update(&row.id, title.as_deref(), None, None, None);
        }
        let dir = self.meeting_dir(&row.id);
        let _ = std::fs::create_dir_all(dir.join("segments"));
        let watcher_stop = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared {
            id: row.id.clone(),
            t0,
            started_at_utc: row.started_at_utc.clone(),
            me_name: (self.deps.me_name)(),
            clock: Mutex::new(Clock::default()),
            capture_mode: Mutex::new(mode),
            speaking: [AtomicBool::new(false), AtomicBool::new(false)],
            queue: Mutex::new(Queue::default()),
            queue_cv: Condvar::new(),
            timeline: Mutex::new(SpeakerTimeline::default()),
            has_observations: AtomicBool::new(false),
            gate: Mutex::new(DictationGate::default()),
            recent_them: Mutex::new(VecDeque::new()),
            recent_me: Mutex::new(VecDeque::new()),
            them_pos: AtomicU64::new(0),
            them_seen: Mutex::new(None),
            me_pos: AtomicU64::new(0),
            me_open_since: AtomicU64::new(u64::MAX),
            abort: AtomicBool::new(false),
            published: AtomicBool::new(false),
            listener: self.listener.clone(),
            dir: dir.clone(),
            max_backlog_ms: self.max_backlog_ms.load(Ordering::Relaxed),
            watcher: Mutex::new(None),
            watcher_stop: watcher_stop.clone(),
            diagnostics: Mutex::new(None),
        });
        if (self.deps.save_diagnostics)() {
            match diagnostics::Writer::create(&dir, &row.id, &row.started_at_utc, TEAMS_LAG_MS) {
                Ok(w) => *shared.diagnostics.lock() = Some(w),
                Err(e) => tracing::warn!(error_kind = ?e.kind(), "could not create the call diagnostics file"),
            }
        }
        let (ctl_tx, ctl_rx) = crossbeam_channel::unbounded();
        let (obs_tx, obs_rx) = crossbeam_channel::unbounded();
        let wavs = ContinuousWav::create(&dir.join("me.wav")).and_then(|a| Ok((a, ContinuousWav::create(&dir.join("them.wav"))?)));
        let (me_wav, them_wav) = match wavs {
            Ok(w) => w,
            Err(e) => {
                // Nothing was recorded: don't leave a "recording" row behind.
                tracing::warn!(error = %e, "cannot create the meeting recordings");
                mic.stop();
                teams.stop();
                let _ = self.deps.store.meeting_delete(&row.id, &self.deps.meetings_dir);
                return Err("Wren couldn't create the meeting recording files.".into());
            }
        };
        // Lock order: dictation → running. Held from seeding the gate until the meeting is published, so
        // the worker never sees a clip before the gate holds every dictation since `t0`, and a dictation
        // starting or ending in between is never lost.
        let dl = self.dictation.lock();
        {
            let mut gate = shared.gate.lock();
            for &(b, e) in dl.iter() {
                if e.is_some_and(|e| e < t0) {
                    continue;
                }
                gate.begin(b.saturating_duration_since(t0).as_millis() as u64);
                if let Some(e) = e {
                    gate.end(e.saturating_duration_since(t0).as_millis() as u64);
                }
            }
        }
        let pipeline = {
            let shared = shared.clone();
            let store = self.deps.store.clone();
            std::thread::Builder::new()
                .name("wren-meeting".into())
                .spawn(move || {
                    Pipeline {
                        shared,
                        store,
                        sources: [mic, teams],
                        wavs: [Some(me_wav), Some(them_wav)],
                        segmenters: Default::default(),
                        reconciled_mute_at: None,
                    }
                    .run(me_rx, them_rx, mode_rx, obs_rx, ctl_rx)
                })
                .map_err(|e| e.to_string())?
        };
        if let Some(make) = &self.deps.make_teams_reader {
            let s = shared.clone();
            let clock: Arc<dyn Fn() -> u64 + Send + Sync> = Arc::new(move || s.now_ms());
            match spawn_watcher(make(), TEAMS_POLL, clock, obs_tx.clone(), watcher_stop.clone()) {
                Ok(h) => *shared.watcher.lock() = Some(h),
                Err(e) => tracing::warn!(error = %e, "cannot start the Teams window watcher"),
            }
        }
        let worker = {
            let shared = shared.clone();
            let store = self.deps.store.clone();
            let transcriber = self.deps.transcriber.clone();
            let bias = self.deps.bias_terms.clone();
            let permit = self.asr_permit.clone();
            std::thread::Builder::new()
                .name("wren-meeting-asr".into())
                .spawn(move || worker(shared, store, transcriber, bias, permit))
        };
        let mut recording = Running {
            shared: shared.clone(),
            ctl: ctl_tx,
            obs: obs_tx,
            pipeline: Some(pipeline),
            worker: None,
            watcher_stop,
        };
        match worker {
            Ok(worker) => recording.worker = Some(worker),
            Err(e) => {
                drop(dl);
                self.finish_recording(recording, Duration::ZERO);
                return Err(e.to_string());
            }
        }
        let quitting = self.quitting.lock();
        if quitting.is_some() {
            drop(quitting);
            drop(dl);
            self.finish_recording(recording, Duration::ZERO);
            return self.deps.store.meeting_get(&row.id).map_err(|e| e.to_string())?
                .ok_or_else(|| "That meeting no longer exists.".to_string());
        }
        tracing::info!(meeting = %row.id, mode = ?mode, "meeting started");
        {
            let mut running = self.running.lock();
            *running = Some(recording);
            shared.published.store(true, Ordering::Release);
        }
        drop(quitting);
        drop(dl);
        shared.emit(MeetingEvent::Status { active: Some(shared.snapshot()) });
        Ok(row)
    }

    fn ctl_ack(&self, make: impl FnOnce(Sender<()>) -> Ctl) -> Result<(), String> {
        let tx = {
            let r = self.running.lock();
            let r = r.as_ref().ok_or("No meeting is being recorded.")?;
            r.ctl.clone()
        };
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        tx.send(make(ack_tx)).map_err(|_| "The meeting recorder stopped.".to_string())?;
        ack_rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "The meeting recorder didn't respond.".to_string())
    }

    pub fn pause(&self) -> Result<(), String> {
        self.ctl_ack(Ctl::Pause)
    }

    pub fn resume(&self) -> Result<(), String> {
        self.ctl_ack(Ctl::Resume)
    }

    /// Feed one Teams observation (the watcher thread and tests).
    pub fn observe(&self, o: TeamsObservation) {
        if let Some(r) = self.running.lock().as_ref() {
            let _ = r.obs.send(o);
        }
    }

    pub fn dictation_began(&self, at: Instant) {
        let mut dl = self.dictation.lock();
        if !matches!(dl.back(), Some((_, None))) {
            dl.push_back((at, None));
        }
        while dl.len() > 1 && dl.front().is_some_and(|(_, e)| e.is_some_and(|e| e.elapsed() > Duration::from_secs(120))) {
            dl.pop_front();
        }
        if let Some(r) = self.running.lock().as_ref() {
            let ms = at.saturating_duration_since(r.shared.t0).as_millis() as u64;
            r.shared.gate.lock().begin(ms);
        }
    }

    pub fn dictation_ended(&self, at: Instant) {
        let mut dl = self.dictation.lock();
        if let Some((_, e @ None)) = dl.back_mut() {
            *e = Some(at);
        }
        if let Some(r) = self.running.lock().as_ref() {
            let ms = at.saturating_duration_since(r.shared.t0).as_millis() as u64;
            r.shared.gate.lock().end(ms);
        }
    }

    #[cfg(test)]
    pub(crate) fn dictation_began_at_ms(&self, ms: u64) {
        if let Some(r) = self.running.lock().as_ref() {
            r.shared.gate.lock().begin(ms);
        }
    }

    #[cfg(test)]
    pub(crate) fn dictation_ended_at_ms(&self, ms: u64) {
        if let Some(r) = self.running.lock().as_ref() {
            r.shared.gate.lock().end(ms);
        }
    }

    /// Stop: finish the audio, wait up to `drain_timeout` for queued clips, save the rest as
    /// "[not transcribed]", mark the meeting ended. Returns its id.
    pub fn stop(&self) -> Option<String> {
        self.stop_inner(self.deps.drain_timeout, true)
    }

    /// `stop` with a custom wait for queued clips.
    pub fn stop_within(&self, drain: Duration) -> Option<String> {
        self.stop_inner(drain, true)
    }

    /// Stop because Wren is quitting: short wait, and no enhanced-notes request (it would be killed
    /// mid-stream and paid for nothing). The notes can be written after the next launch.
    pub fn stop_for_quit(&self, drain: Duration) -> Option<String> {
        self.stop_inner(drain, false)
    }

    fn stop_inner(&self, drain: Duration, run_hook: bool) -> Option<String> {
        if !run_hook {
            // Keep the deadline latched through app exit, even if a start outlasts this wait.
            {
                let mut q = self.quitting.lock();
                let deadline = Instant::now() + drain;
                *q = Some(q.map_or(deadline, |d| d.min(deadline)));
            }
            let until = Instant::now() + QUIT_WAITS_FOR_START;
            while self.starting.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let taken = {
            let mut running = self.running.lock();
            let r = running.take();
            if let Some(r) = &r {
                *self.stopping.lock() = Some(r.shared.id.clone());
            }
            r
        };
        let Some(r) = taken else {
            if !run_hook {
                // Quitting while an earlier Stop still saves: wait for it rather than cut it short.
                let mut st = self.stopping.lock();
                let until = Instant::now() + drain + Duration::from_secs(5);
                while st.is_some() && Instant::now() < until {
                    self.stopped_cv.wait_until(&mut st, until);
                }
            }
            return None;
        };
        struct Stopping<'a>(&'a Mutex<Option<String>>, &'a Condvar);
        impl Drop for Stopping<'_> {
            fn drop(&mut self) {
                *self.0.lock() = None;
                self.1.notify_all();
            }
        }
        let stopping_guard = Stopping(&self.stopping, &self.stopped_cv);
        let s = self.finish_recording(r, drain);
        let quitting = self.quitting.lock().is_some(); // read before quit can see this stop finish
        drop(stopping_guard); // saved: a new meeting may start, quit may proceed
        s.emit(MeetingEvent::Status { active: None });
        let hook = self.on_stopped.read().clone();
        if let (true, false, Some(h)) = (run_hook, quitting, hook) {
            h(s.id.clone());
        }
        Some(s.id.clone())
    }

    fn finish_recording(&self, mut r: Running, drain: Duration) -> Arc<Shared> {
        let s = r.shared.clone();
        let elapsed = s.elapsed_ms();
        r.watcher_stop.store(true, Ordering::Relaxed);
        let _ = r.ctl.send(Ctl::Stop);
        if let Some(p) = r.pipeline.take() {
            let _ = p.join();
        }
        let deadline = Instant::now() + drain;
        {
            let mut q = s.queue.lock();
            loop {
                // Quit (from another thread) may shorten the wait.
                let until = self.quitting.lock().map_or(deadline, |d| d.min(deadline));
                if (q.jobs.is_empty() && q.in_flight.is_none()) || Instant::now() >= until {
                    break;
                }
                s.queue_cv.wait_until(&mut q, until.min(Instant::now() + Duration::from_millis(100)));
            }
            q.closed = true;
            s.abort.store(true, Ordering::SeqCst);
            let mut left: Vec<(Source, u64, u64)> = q.jobs.drain(..).map(|j| (j.source, j.start_ms, j.end_ms)).collect();
            if let Some((_, src, a, b)) = q.in_flight.take() {
                left.push((src, a, b));
            }
            s.queue_cv.notify_all();
            drop(q);
            left.sort_by_key(|x| x.1);
            if !left.is_empty() {
                tracing::warn!(meeting = %s.id, clips = left.len(), "meeting stopped before every clip was transcribed");
            }
            for (src, a, b) in left {
                let speaker = (src == Source::Me).then(|| s.me_name.clone());
                let seg = NewSegment { meeting_id: s.id.clone(), source: src, speaker, confidence: 0.0, muted: false, start_ms: a, end_ms: b, text: NOT_TRANSCRIBED.into() };
                if let Ok(row) = self.deps.store.segment_insert(&seg) {
                    s.emit(MeetingEvent::Segment { segment: row });
                }
            }
        }
        if let Some(w) = r.worker.take() {
            // On a clean stop the worker exits within moments of `closed`; give it that time.
            let until = Instant::now() + Duration::from_millis(300);
            while !w.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if w.is_finished() {
                let _ = w.join();
                let _ = std::fs::remove_dir_all(s.dir.join("segments"));
            }
            // Otherwise it's inside a slow transcription: it discards its result (abort), deletes its
            // own clip and exits; the 24 h sweep removes the folder.
        }
        if let Err(e) = self.deps.store.meeting_set_status(&s.id, MeetingStatus::Ended, Some(chrono::Utc::now()), elapsed) {
            tracing::warn!(error = %e, "could not mark the meeting ended");
        }
        tracing::info!(meeting = %s.id, elapsed_ms = elapsed, "meeting stopped");
        s
    }
}

impl Drop for MeetingService {
    /// Leaving without `stop` (crash path, tests): finish the WAVs; the row stays `recording` and
    /// becomes `interrupted` on the next launch.
    fn drop(&mut self) {
        if let Some(mut r) = self.running.lock().take() {
            r.watcher_stop.store(true, Ordering::Relaxed);
            r.shared.abort.store(true, Ordering::SeqCst);
            {
                let mut q = r.shared.queue.lock();
                q.closed = true;
                q.jobs.clear();
                r.shared.queue_cv.notify_all();
            }
            let _ = r.ctl.send(Ctl::Stop);
            if let Some(p) = r.pipeline.take() {
                let _ = p.join();
            }
        }
    }
}

struct Pipeline {
    shared: Arc<Shared>,
    store: Arc<Store>,
    sources: [Box<dyn AudioSource>; 2],
    wavs: [Option<ContinuousWav>; 2],
    segmenters: [Option<Segmenter>; 2],
    reconciled_mute_at: Option<u64>,
}

impl Pipeline {
    fn segmenter(&mut self, i: usize) -> &mut Segmenter {
        self.segmenters[i].get_or_insert_with(|| Segmenter::new(SegmenterConfig::default(), SAMPLE_RATE))
    }

    fn enqueue(&self, source: Source, seg: RawSegment) {
        let mut q = self.shared.queue.lock();
        if q.closed {
            return;
        }
        let seq = q.next_seq;
        q.next_seq += 1;
        q.jobs.push_back(Job { seq, source, start_ms: seg.start_ms, end_ms: seg.end_ms, samples: seg.samples, queued_at: Instant::now() });
        // Memory bound: when transcription falls far behind, the oldest waiting clips become gaps.
        let mut dropped = Vec::new();
        while q.jobs.iter().map(|j| j.end_ms - j.start_ms).sum::<u64>() > self.shared.max_backlog_ms && q.jobs.len() > 1 {
            if let Some(j) = q.jobs.pop_front() {
                dropped.push((j.source, j.start_ms, j.end_ms));
            }
        }
        self.shared.queue_cv.notify_all();
        drop(q);
        if !dropped.is_empty() {
            tracing::warn!(meeting = %self.shared.id, clips = dropped.len(), "transcription fell too far behind; oldest clips saved as gaps");
        }
        for (src, a, b) in dropped {
            let speaker = (src == Source::Me).then(|| self.shared.me_name.clone());
            let seg = NewSegment { meeting_id: self.shared.id.clone(), source: src, speaker, confidence: 0.0, muted: false, start_ms: a, end_ms: b, text: NOT_TRANSCRIBED.into() };
            if let Ok(row) = self.store.segment_insert(&seg) {
                self.shared.emit(MeetingEvent::Segment { segment: row });
            }
        }
    }

    fn set_speaking(&self, source: Source, on: bool) {
        let flag = &self.shared.speaking[idx(source)];
        if flag.swap(on, Ordering::Relaxed) != on {
            self.shared.emit(MeetingEvent::Speaking { meeting_id: self.shared.id.clone(), source, on });
        }
    }

    fn block(&mut self, source: Source, b: AudioBlock) {
        let i = idx(source);
        if source == Source::Them {
            let end = b.stream_pos_ms + b.samples.len() as u64 * 1000 / SAMPLE_RATE as u64;
            self.shared.them_pos.fetch_max(end, Ordering::Relaxed);
            *self.shared.them_seen.lock() = Some(Instant::now());
        }
        if self.shared.paused() {
            if let Some(w) = self.wavs[i].as_mut() {
                w.write(&vec![0.0; b.samples.len()]);
            }
            return;
        }
        if let Some(w) = self.wavs[i].as_mut() {
            w.write(&b.samples);
        }
        let segs = self.segmenter(i).push(&b.samples, b.stream_pos_ms);
        for s in segs {
            self.enqueue(source, s);
        }
        if source == Source::Me {
            let end = b.stream_pos_ms + b.samples.len() as u64 * 1000 / SAMPLE_RATE as u64;
            let open = self.segmenter(i).open_since().unwrap_or(u64::MAX);
            self.shared.me_open_since.store(open, Ordering::Relaxed);
            self.shared.me_pos.store(end, Ordering::Relaxed);
        }
        let speaking = self.segmenter(i).is_speaking();
        self.set_speaking(source, speaking);
    }

    fn flush(&mut self) {
        // No more mic audio is coming: nothing for a Them clip to wait for.
        self.shared.me_open_since.store(u64::MAX, Ordering::Relaxed);
        self.shared.me_pos.store(u64::MAX, Ordering::Relaxed);
        for (i, source) in [(0, Source::Me), (1, Source::Them)] {
            if let Some(seg) = self.segmenters[i].as_mut().and_then(Segmenter::flush) {
                self.enqueue(source, seg);
            }
            self.set_speaking(source, false);
        }
    }

    fn observation(&mut self, o: TeamsObservation) {
        let s = &self.shared;
        let ((before, title_before, me_before), (after, title, me)) = {
            let mut t = s.timeline.lock();
            let before = (t.participants(), t.latest_title(), t.self_name());
            t.record(&o);
            (before, (t.participants(), t.latest_title(), t.self_name()))
        };
        self.shared.diagnose(|w| w.reading(&o));
        self.reconcile_mute();
        let s = &self.shared;
        s.has_observations.store(true, Ordering::Relaxed);
        s.queue_cv.notify_all();
        if after != before || me != me_before {
            let _ = self.store.meeting_update(&s.id, None, None, Some(&after), me.as_deref());
        }
        if after != before {
            s.emit(MeetingEvent::Participants { meeting_id: s.id.clone(), names: after });
        }
        if title != title_before {
            if let Some(t) = title {
                if matches!(self.store.meeting_set_auto_title(&s.id, &t), Ok(true)) {
                    s.emit(MeetingEvent::Title { meeting_id: s.id.clone(), title: t });
                }
            }
        }
    }

    fn reconcile_mute(&mut self) {
        let s = &self.shared;
        let first = s.timeline.lock().first_mute_at();
        let Some(at) = first.filter(|at| Some(*at) != self.reconciled_mute_at) else { return };
        // The worker classifies and saves under this lock. If it saved before the first read,
        // correct its row here, and emit the correction after the original event.
        let _queue = s.queue.lock();
        match self.store.mute_me_segments(&s.id, at.saturating_sub(UNKNOWN_BEFORE_FIRST_MUTE_READ_MS), at) {
            Ok(rows) => {
                self.reconciled_mute_at = Some(at);
                for segment in rows {
                    s.emit(MeetingEvent::Segment { segment });
                }
            }
            Err(e) => tracing::error!(error = %e, "could not reconcile microphone lines with Teams mute state"),
        }
    }

    fn run(
        mut self,
        mut me_rx: Receiver<AudioBlock>,
        mut them_rx: Receiver<AudioBlock>,
        mut mode_rx: Receiver<CaptureMode>,
        mut obs_rx: Receiver<TeamsObservation>,
        ctl_rx: Receiver<Ctl>,
    ) {
        let tick = crossbeam_channel::tick(STATUS_EVERY);
        loop {
            select! {
                // A closed channel (a source that failed or ended) is always "ready": swap it for
                // `never()` or this loop would spin a CPU core for the rest of the meeting.
                recv(me_rx) -> b => match b {
                    Ok(b) => self.block(Source::Me, b),
                    Err(_) => me_rx = crossbeam_channel::never(),
                },
                recv(them_rx) -> b => match b {
                    Ok(b) => self.block(Source::Them, b),
                    Err(_) => them_rx = crossbeam_channel::never(),
                },
                recv(obs_rx) -> o => match o {
                    Ok(o) => self.observation(o),
                    Err(_) => obs_rx = crossbeam_channel::never(),
                },
                recv(mode_rx) -> m => match m {
                    Ok(m) => {
                        *self.shared.capture_mode.lock() = m;
                        let _ = self.store.meeting_set_capture_mode(&self.shared.id, m);
                        self.shared.emit(MeetingEvent::Status { active: Some(self.shared.snapshot()) });
                    }
                    Err(_) => mode_rx = crossbeam_channel::never(),
                },
                recv(tick) -> _ => self.shared.emit(MeetingEvent::Status { active: Some(self.shared.snapshot()) }),
                recv(ctl_rx) -> c => match c {
                    Ok(Ctl::Pause(ack)) => {
                        // Everything already received belongs before the pause.
                        while let Ok(b) = me_rx.try_recv() { self.block(Source::Me, b) }
                        while let Ok(b) = them_rx.try_recv() { self.block(Source::Them, b) }
                        self.flush();
                        self.shared.clock.lock().paused_since.get_or_insert_with(Instant::now);
                        self.shared.emit(MeetingEvent::Status { active: Some(self.shared.snapshot()) });
                        let _ = ack.send(());
                    }
                    Ok(Ctl::Resume(ack)) => {
                        {
                            let mut c = self.shared.clock.lock();
                            if let Some(p) = c.paused_since.take() { c.paused_total += p.elapsed(); }
                        }
                        self.shared.emit(MeetingEvent::Status { active: Some(self.shared.snapshot()) });
                        let _ = ack.send(());
                    }
                    Ok(Ctl::Stop) | Err(_) => break,
                },
            }
        }
        for s in self.sources.iter_mut() {
            s.stop();
        }
        while let Ok(b) = me_rx.try_recv() {
            self.block(Source::Me, b);
        }
        while let Ok(b) = them_rx.try_recv() {
            self.block(Source::Them, b);
        }
        self.flush();
        for w in self.wavs.iter_mut() {
            if let Some(w) = w.take() {
                if let Err(e) = w.finalize() {
                    tracing::warn!(error = %e, "could not finish a meeting recording");
                }
            }
        }
        // Finish the current Teams read before draining its observations. The last read may show
        // the mute state for the first time, and must be applied before Stop can request notes.
        // Release the old reader before the next recorder starts (its macOS cleanup changes AX).
        let watcher = self.shared.watcher.lock().take();
        if let Some(w) = watcher {
            let until = Instant::now() + WATCHER_JOIN;
            while !w.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if w.is_finished() {
                let _ = w.join();
            } else {
                tracing::warn!(meeting = %self.shared.id, "Teams window read still running after stop; leaving it to finish");
            }
        }
        while let Ok(o) = obs_rx.try_recv() {
            self.observation(o);
        }
        self.reconcile_mute();
    }
}

/// Pick the next job: oldest first, except a Me clip waits for the Them stream to pass its end (so
/// bleed can be checked) and goes after any overlapping Them clip already queued.
fn next_job(s: &Shared, q: &mut Queue) -> Option<Job> {
    let them_live = s.them_seen.lock().is_some_and(|t| t.elapsed() < Duration::from_secs(1));
    let them_pos = s.them_pos.load(Ordering::Relaxed);
    let mut pick = None;
    let may_echo = |me: &Job, them: &Job| {
        me.start_ms < them.end_ms && them.start_ms < me.end_ms + ECHO_MAX_LAG_MS && starts_after(them.start_ms, me.start_ms)
    };
    for (i, j) in q.jobs.iter().enumerate() {
        if j.source == Source::Them {
            // A Them clip that starts clearly after a queued Me clip may be Teams echoing it: the Me
            // line goes first so the echo can be checked against it.
            let echo_of_queued_me = q
                .jobs
                .iter()
                .any(|m| m.source == Source::Me && may_echo(m, j));
            // Or one the mic hasn't delivered yet: still being spoken, or not reached yet. Bounded.
            let me_open = s.me_open_since.load(Ordering::Relaxed);
            let me_pending = !q.closed
                && (s.me_pos.load(Ordering::Relaxed) < j.start_ms
                || (me_open != u64::MAX && starts_after(j.start_ms, me_open)))
                && j.queued_at.elapsed() < ME_WAIT_FOR_THEM;
            if echo_of_queued_me || me_pending {
                continue;
            }
            pick = Some(i);
            break;
        }
        // Me waits for overlapping Them clips that could be bleed (not ones that start clearly later).
        let overlapping_them = q.jobs.iter().any(|t| {
            t.source == Source::Them && t.start_ms < j.end_ms && j.start_ms < t.end_ms && !starts_after(t.start_ms, j.start_ms)
        });
        let waiting = them_live && them_pos < j.end_ms + 100 && j.queued_at.elapsed() < ME_WAIT_FOR_THEM;
        // Empty window reads don't tell us whether the mic was muted. Outside a call only the
        // opening clips wait: otherwise we'd add three seconds to every line.
        let unread = !q.closed
            && j.queued_at.elapsed() < ME_WAIT_FOR_TEAMS
            && s.teams_watching()
            && match s.timeline.lock().latest_mute_at() {
                Some(at) => at < j.end_ms,
                None => j.start_ms < UNKNOWN_BEFORE_FIRST_MUTE_READ_MS,
            };
        if overlapping_them || waiting || unread {
            continue;
        }
        pick = Some(i);
        break;
    }
    pick.and_then(|i| q.jobs.remove(i))
}

fn worker(
    s: Arc<Shared>,
    store: Arc<Store>,
    transcriber: Arc<dyn Transcriber>,
    bias: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    permit: Arc<Mutex<()>>,
) {
    loop {
        let job = {
            let mut q = s.queue.lock();
            loop {
                if q.closed {
                    return;
                }
                if let Some(j) = next_job(&s, &mut q) {
                    q.in_flight = Some((j.seq, j.source, j.start_ms, j.end_ms));
                    break j;
                }
                s.queue_cv.wait_for(&mut q, Duration::from_millis(100));
            }
        };
        let row = process(&s, transcriber.as_ref(), &bias, &job, &permit);
        // The clip stays "in flight" until its row is saved, so Stop (and the notes it triggers)
        // never runs between transcription and the write.
        let mut q = s.queue.lock();
        if s.abort.load(Ordering::SeqCst) || q.in_flight.map(|f| f.0) != Some(job.seq) {
            // Stop already saved this clip as "[not transcribed]".
            return;
        }
        if let Some(mut seg) = row {
            if seg.source == Source::Me {
                seg.muted = s.timeline.lock().muted_during(seg.start_ms, seg.end_ms).unwrap_or(false);
            }
            match store.segment_insert(&seg) {
                Ok(row) => {
                    s.diagnose(|w| {
                        w.line(&diagnostics::RecordedLine {
                            source: match row.source {
                                Source::Me => "me".into(),
                                Source::Them => "them".into(),
                            },
                            start_ms: row.start_ms,
                            end_ms: row.end_ms,
                            speaker: row.speaker.clone(),
                            confidence: row.confidence,
                            muted: row.muted,
                            text: row.text.clone(),
                        })
                    });
                    let mut r = match row.source {
                        Source::Them => s.recent_them.lock(),
                        Source::Me => s.recent_me.lock(),
                    };
                    r.push_back((row.start_ms, row.end_ms, row.text.clone()));
                    while r.front().is_some_and(|f| f.1 + RECENT_THEM_MS < row.end_ms) {
                        r.pop_front();
                    }
                    drop(r);
                    s.emit(MeetingEvent::Segment { segment: row });
                }
                Err(e) => tracing::warn!(error = %e, "could not save a meeting segment"),
            }
        }
        q.in_flight = None;
        s.queue_cv.notify_all();
    }
}

/// Transcribe and label one clip; `None` when it's dropped (silence, dictation, bleed-through).
fn process(
    s: &Shared,
    transcriber: &dyn Transcriber,
    bias: &Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    j: &Job,
    permit: &Mutex<()>,
) -> Option<NewSegment> {
    if j.source == Source::Me && s.gate.lock().blocks(j.start_ms, j.end_ms) {
        return None;
    }
    let mut terms = bias();
    terms.extend(s.timeline.lock().participants());
    let clip = s.dir.join("segments").join(format!("{}.wav", j.seq));
    let clip_ms = j.end_ms - j.start_ms;
    let text = match write_clip(&clip, &j.samples) {
        Ok(()) => {
            let r = {
                let _permit = permit.lock();
                let first = transcriber.transcribe(&clip, clip_ms, &terms);
                first.or_else(|e| {
                    if s.abort.load(Ordering::SeqCst) {
                        return Err(e);
                    }
                    tracing::warn!(error = %e, "meeting clip transcription failed; retrying once");
                    transcriber.transcribe(&clip, clip_ms, &terms)
                })
            };
            let _ = std::fs::remove_file(&clip);
            match r {
                Ok(t) => t.trim().to_string(),
                Err(e) => {
                    tracing::warn!(error = %e, "meeting clip could not be transcribed");
                    NOT_TRANSCRIBED.to_string()
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not write a meeting clip");
            NOT_TRANSCRIBED.to_string()
        }
    };
    if text.is_empty() {
        return None;
    }
    let mut seg = NewSegment {
        meeting_id: s.id.clone(),
        source: j.source,
        speaker: None,
        confidence: 0.0,
        muted: false,
        start_ms: j.start_ms,
        end_ms: j.end_ms,
        text,
    };
    match j.source {
        Source::Me => {
            if s.gate.lock().blocks(j.start_ms, j.end_ms) {
                return None;
            }
            let them: Vec<(u64, u64, String)> = s.recent_them.lock().iter().cloned().collect();
            let refs: Vec<(u64, u64, &str)> = them.iter().map(|(a, b, t)| (*a, *b, t.as_str())).collect();
            if is_bleed(&(j.start_ms, j.end_ms, seg.text.as_str()), &refs) {
                return None;
            }
            seg.speaker = Some(s.me_name.clone());
            seg.confidence = 1.0;
        }
        Source::Them => {
            let me: Vec<(u64, u64, String)> = s.recent_me.lock().iter().cloned().collect();
            let refs: Vec<(u64, u64, &str)> = me.iter().map(|(a, b, t)| (*a, *b, t.as_str())).collect();
            if is_echo(&(j.start_ms, j.end_ms, seg.text.as_str()), &refs) {
                tracing::debug!(start_ms = j.start_ms, "dropped Teams echo of the user's own line");
                return None;
            }
            if s.has_observations.load(Ordering::Relaxed) {
                let want = j.end_ms + TEAMS_LAG_MS;
                let until = Instant::now() + THEM_WAIT_FOR_TEAMS;
                while s.now_ms() < want && Instant::now() < until && !s.abort.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let a = s.timeline.lock().attribute(j.start_ms, j.end_ms, TEAMS_LAG_MS);
            seg.speaker = a.name;
            seg.confidence = a.confidence;
        }
    }
    Some(seg)
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
