//! Tauri glue for meetings (plan Task 8): the service wired to the real engine, mic and Teams; the
//! commands the UI calls; one event channel (`meeting://event`); the tray's meeting items.

use crate::meetings::capture::mic::MicSource;
use crate::meetings::capture::AudioSource;
use crate::meetings::session::{ActiveMeeting, Deps, MeetingEvent, MeetingService, SourcePair, Transcriber};
use crate::meetings::{Length, MeetingRow, MeetingStatus, NoteRow, SegmentRow, Source};
use crate::session::app::AppEnv;
use crate::store::{Store, StoreResult};
use crate::AppState;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// Every meeting event, payload [`MeetingEvent`] (tagged by `kind`).
pub const EVENT: &str = "meeting://event";
/// Enhanced-notes progress, payload [`crate::meetings::enhance::NoteEvent`] (tagged by `event`).
pub const NOTE_EVENT: &str = "meeting://note";
/// Notes being written now, as (meeting id, length): one run per pair at a time.
static GENERATING: parking_lot::Mutex<Vec<(String, Length)>> = parking_lot::Mutex::new(Vec::new());
/// Quit was chosen while a meeting records: the page asks "Stop meeting notes and quit?".
pub const EVENT_CONFIRM_QUIT: &str = "meeting://confirm-quit";
/// Set once the user confirmed "Stop meeting notes and quit?" (lets the exit through).
pub static QUIT_CONFIRMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a meeting is being recorded now.
pub fn recording(app: &AppHandle) -> bool {
    app.try_state::<Meetings>().is_some_and(|m| m.0.active().is_some())
}

/// Quit was chosen mid-meeting: show Wren and ask "Stop meeting notes and quit?".
pub fn ask_before_quit(app: &AppHandle) {
    crate::tray::show_main(app);
    let _ = app.emit(EVENT_CONFIRM_QUIT, ());
}
/// Stop waits this long for queued clips before marking the rest "[not transcribed]".
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// When quitting, wait less.
const QUIT_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Playing a line plays its stretch plus this much after it.
const CLIP_TAIL_MS: u64 = 500;
/// Wait this long for the speech engine to be ready before giving up on a clip.
const ENGINE_READY_WAIT: Duration = Duration::from_secs(30);

/// Managed state: the meeting service (absent if the services failed to start).
pub struct Meetings(pub Arc<MeetingService>);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingDetail {
    pub meeting: MeetingRow,
    pub segments: Vec<SegmentRow>,
    pub notes: Vec<NoteRow>,
}

pub fn detail(store: &Store, id: &str) -> StoreResult<Option<MeetingDetail>> {
    let Some(meeting) = store.meeting_get(id)? else { return Ok(None) };
    Ok(Some(MeetingDetail { meeting, segments: store.segments(id)?, notes: store.notes(id)? }))
}

/// The dictation engine as a meeting transcriber: same model, same hallucination guards.
pub struct EngineTranscriber(pub Arc<AppEnv>);

impl Transcriber for EngineTranscriber {
    fn transcribe(&self, wav: &Path, clip_ms: u64, bias: &[String]) -> Result<String, String> {
        use crate::constants::{TRANSCRIBE_TIMEOUT_MS, TRANSCRIBE_TIMEOUT_PER_CLIP};
        use crate::engine::{EngineStatus, TranscribeRequest};
        let until = Instant::now() + ENGINE_READY_WAIT;
        while !matches!(self.0.engine().status(), EngineStatus::Ready { .. }) {
            if Instant::now() > until {
                return Err("the speech engine isn't ready".into());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        let req = TranscribeRequest {
            id: uuid::Uuid::new_v4().to_string(),
            audio_path: wav.to_path_buf(),
            bias_terms: bias.to_vec(),
            prompt: String::new(),
        };
        let timeout = Duration::from_millis(TRANSCRIBE_TIMEOUT_MS + (clip_ms as f64 * TRANSCRIBE_TIMEOUT_PER_CLIP) as u64);
        let r = self.0.engine().transcribe(req, timeout).map_err(|e| e.to_string())?;
        let glossary = if bias.is_empty() { String::new() } else { format!("Glossary: {}.", bias.join(", ")) };
        let guarded = crate::pipeline::guards::apply(&r.text, r.speech_ms, clip_ms, &glossary);
        Ok(crate::pipeline::tidy::normalise_whitespace(&guarded.text))
    }
}

fn make_sources(app: &AppHandle) -> SourcePair {
    let device = app.state::<AppState>().settings.read().mic_device.clone();
    let mic: Box<dyn AudioSource> = Box::new(MicSource::new(device));
    #[cfg(windows)]
    let teams: Box<dyn AudioSource> = Box::new(crate::meetings::capture::win_loopback::TeamsLoopback::new(
        crate::meetings::capture::win_loopback::Target::Teams,
    ));
    #[cfg(target_os = "macos")]
    let teams: Box<dyn AudioSource> = Box::new(crate::meetings::capture::mac_tap::TeamsTap::new(
        crate::meetings::capture::mac_tap::Target::Teams,
    ));
    #[cfg(not(any(windows, target_os = "macos")))]
    let teams: Box<dyn AudioSource> = Box::new(crate::meetings::capture::Silence::default());
    (mic, teams)
}

/// Build the service, hook its events to the UI and tray, and register it for dictation notices.
pub fn build_service(app: &AppHandle, env: Arc<AppEnv>) -> Arc<MeetingService> {
    let state = app.state::<AppState>();
    let a1 = app.clone();
    let a2 = app.clone();
    let a3 = app.clone();
    #[cfg(windows)]
    let make_teams_reader: Option<Arc<dyn Fn() -> crate::meetings::teams::ReaderFactory + Send + Sync>> =
        Some(Arc::new(|| {
            Box::new(|| {
                crate::meetings::teams::uia::UiaTeamsReader::new()
                    .ok()
                    .map(|r| Box::new(r) as Box<dyn crate::meetings::teams::TeamsReader>)
            }) as crate::meetings::teams::ReaderFactory
        }));
    #[cfg(target_os = "macos")]
    let make_teams_reader: Option<Arc<dyn Fn() -> crate::meetings::teams::ReaderFactory + Send + Sync>> =
        Some(Arc::new(|| {
            Box::new(|| {
                Some(Box::new(crate::meetings::teams::ax_macos::AxTeamsReader::new())
                    as Box<dyn crate::meetings::teams::TeamsReader>)
            }) as crate::meetings::teams::ReaderFactory
        }));
    #[cfg(not(any(windows, target_os = "macos")))]
    let make_teams_reader = None;
    let deps = Deps {
        store: state.store.clone(),
        transcriber: Arc::new(EngineTranscriber(env)),
        meetings_dir: state.paths.meetings_dir(),
        make_sources: Arc::new(move || make_sources(&a1)),
        make_teams_reader,
        bias_terms: Arc::new(move || {
            a2.state::<AppState>().store.bias_terms(crate::constants::BIAS_TERMS_MAX).unwrap_or_default()
        }),
        me_name: Arc::new(move || meeting_name(&a3)),
        drain_timeout: DRAIN_TIMEOUT,
        // 20 minutes of audio waiting at most (~77 MB of samples).
        max_backlog_ms: 20 * 60_000,
    };
    let svc = Arc::new(MeetingService::new(deps));
    let app_ev = app.clone();
    svc.set_listener(Arc::new(move |e: MeetingEvent| {
        if let MeetingEvent::Status { active } = &e {
            // Queued, never waited on: tray setters hop to the main thread, which may itself be waiting
            // on the recorder (quit, a sync command) — waiting here would deadlock.
            let status = active.as_ref().map(|a| a.status);
            let a = app_ev.clone();
            let _ = app_ev.run_on_main_thread(move || crate::tray::set_meeting_state(&a, status));
        }
        let _ = app_ev.emit(EVENT, e);
    }));
    crate::meetings::register_service(&svc);
    // Spec §5.4: Medium notes start on their own when a meeting stops (if a key is saved).
    let app_stop = app.clone();
    svc.set_on_stopped(Arc::new(move |id: String| {
        if crate::meetings::claude::KeyStore::system().get().is_some() {
            spawn_enhance(&app_stop, id, Length::Medium);
        }
    }));
    svc
}

fn meeting_name(app: &AppHandle) -> String {
    let n = app.state::<AppState>().settings.read().meeting_name.trim().to_string();
    if n.is_empty() { "Me".to_string() } else { n }
}

fn claude_model(app: &AppHandle) -> String {
    let m = app.state::<AppState>().settings.read().claude_model.clone();
    if m == "claude-sonnet-5-5" { m } else { crate::meetings::claude::DEFAULT_MODEL.to_string() }
}

/// Removes a (meeting, length) from `GENERATING` however the run ends (error, panic, spawn failure).
struct Generating(String, Length);
impl Drop for Generating {
    fn drop(&mut self) {
        GENERATING.lock().retain(|(i, l)| !(*i == self.0 && *l == self.1));
    }
}

/// Start writing a note in the background; false if one for this meeting and length is already running.
pub fn spawn_enhance(app: &AppHandle, id: String, len: Length) -> bool {
    {
        let mut g = GENERATING.lock();
        if g.iter().any(|(i, l)| *i == id && *l == len) {
            return false;
        }
        g.push((id.clone(), len));
    }
    let guard = Generating(id.clone(), len);
    let _ = app.emit(NOTE_EVENT, crate::meetings::enhance::NoteEvent::Started { meeting_id: id.clone(), length: len });
    let app = app.clone();
    let spawned = std::thread::Builder::new().name("wren-notes".into()).spawn(move || {
        let _guard = guard;
        let state = app.state::<AppState>();
        let (model, me) = (claude_model(&app), meeting_name(&app));
        let _ = crate::meetings::enhance::generate(
            &state.store,
            &crate::meetings::enhance::ClaudeWriter,
            &model,
            &me,
            &id,
            len,
            &mut |e| {
                let _ = app.emit(NOTE_EVENT, e);
            },
        );
    });
    spawned.is_ok()
}

/// Paint a red "recording" dot into the bottom-right of an RGBA icon.
pub fn recording_dot(rgba: &mut [u8], w: u32, h: u32) {
    let r = (w.min(h) as f32 * 0.22).max(2.0);
    let (cx, cy) = (w as f32 - r - 1.0, h as f32 - r - 1.0);
    for y in 0..h {
        for x in 0..w {
            let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let i = ((y * w + x) * 4) as usize;
            if d <= r + 1.0 && i + 3 < rgba.len() {
                // White ring for contrast on dark trays, red inside.
                let px: [u8; 4] = if d <= r - 0.5 { [229, 57, 53, 255] } else { [255, 255, 255, 255] };
                rgba[i..i + 4].copy_from_slice(&px);
            }
        }
    }
}

/// Longest stretch one ▶ can play (a line is at most ~20 s).
const CLIP_MAX_MS: u64 = 60_000;

/// The stretch `[start_ms, end_ms + CLIP_TAIL_MS)` of a meeting WAV as a playable data URL. Reads the
/// real file length, so lines spoken since the last header update (a live meeting) still play.
pub fn clip_data_url(path: &Path, start_ms: u64, end_ms: u64) -> anyhow::Result<Option<String>> {
    use base64::Engine;
    use std::io::{Read, Seek, SeekFrom};
    if !path.is_file() {
        return Ok(None);
    }
    let (offset, total) = crate::meetings::capture::wav::pcm_layout(path)?;
    let rate = 16_000u64;
    let end_ms = end_ms.max(start_ms).min(start_ms + CLIP_MAX_MS);
    let from = (start_ms * rate / 1000).min(total);
    let to = ((end_ms + CLIP_TAIL_MS) * rate / 1000).min(total).max(from);
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(offset + from * 2))?;
    let mut pcm = vec![0u8; ((to - from) * 2) as usize];
    f.read_exact(&mut pcm)?;
    let spec = hound::WavSpec { channels: 1, sample_rate: rate as u32, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = hound::WavWriter::new(&mut buf, spec)?;
        for c in pcm.chunks_exact(2) {
            w.write_sample(i16::from_le_bytes([c[0], c[1]]))?;
        }
        w.finalize()?;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(buf.into_inner());
    Ok(Some(format!("data:audio/wav;base64,{b64}")))
}

/// Meeting ids are UUIDs we made; anything else from the page is refused before touching files.
fn valid_id(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id).map(|_| ()).map_err(|_| "That meeting doesn't exist.".to_string())
}

fn svc(m: &Option<State<'_, Meetings>>) -> Result<Arc<MeetingService>, String> {
    m.as_ref().map(|s| s.0.clone()).ok_or_else(|| "Meetings aren't available: the speech engine didn't start.".into())
}

/// Run blocking meeting work off the main thread (opening audio, waiting for the recorder).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn meeting_start(app: AppHandle, title: Option<String>) -> Result<MeetingRow, String> {
    let s = svc(&app.try_state::<Meetings>())?;
    blocking(move || s.start(title)).await
}

#[tauri::command]
pub async fn meeting_pause(app: AppHandle) -> Result<(), String> {
    let s = svc(&app.try_state::<Meetings>())?;
    blocking(move || s.pause()).await
}

#[tauri::command]
pub async fn meeting_resume(app: AppHandle) -> Result<(), String> {
    let s = svc(&app.try_state::<Meetings>())?;
    blocking(move || s.resume()).await
}

#[tauri::command]
pub async fn meeting_stop(app: AppHandle) -> Result<Option<String>, String> {
    let s = svc(&app.try_state::<Meetings>())?;
    tauri::async_runtime::spawn_blocking(move || s.stop()).await.map_err(|e| e.to_string())
}

/// The quit confirmation said yes: stop the meeting (short drain), then quit.
#[tauri::command]
pub fn meeting_stop_and_quit(app: AppHandle) {
    std::thread::spawn(move || {
        if let Some(m) = app.try_state::<Meetings>() {
            m.0.stop_for_quit(QUIT_DRAIN_TIMEOUT);
        }
        QUIT_CONFIRMED.store(true, std::sync::atomic::Ordering::SeqCst);
        app.exit(0);
    });
}

#[tauri::command]
pub fn meeting_active(app: AppHandle) -> Option<ActiveMeeting> {
    app.try_state::<Meetings>().and_then(|m| m.0.active())
}

#[tauri::command]
pub fn meeting_list(state: State<'_, AppState>) -> Result<Vec<MeetingRow>, String> {
    state.store.meeting_list().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn meeting_get(state: State<'_, AppState>, id: String) -> Result<Option<MeetingDetail>, String> {
    valid_id(&id)?;
    detail(&state.store, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn meeting_update(state: State<'_, AppState>, id: String, title: Option<String>, my_notes: Option<String>) -> Result<(), String> {
    valid_id(&id)?;
    state.store.meeting_update(&id, title.as_deref(), my_notes.as_deref(), None, None).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn meeting_rename_speaker(
    state: State<'_, AppState>,
    id: String,
    segment_id: Option<i64>,
    from: Option<String>,
    to: String,
) -> Result<usize, String> {
    valid_id(&id)?;
    state.store.segment_rename(&id, segment_id, from.as_deref(), &to).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn meeting_delete(app: AppHandle, state: State<'_, AppState>, id: String) -> Result<(), String> {
    valid_id(&id)?;
    if app.try_state::<Meetings>().is_some_and(|m| m.0.is_busy(&id)) {
        return Err("Stop this meeting before deleting it.".into());
    }
    state.store.meeting_delete(&id, &state.paths.meetings_dir()).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn meeting_audio_clip(
    state: State<'_, AppState>,
    id: String,
    source: Source,
    start_ms: u64,
    end_ms: u64,
) -> Result<Option<String>, String> {
    valid_id(&id)?;
    let file = match source {
        Source::Me => "me.wav",
        Source::Them => "them.wav",
    };
    let path = state.paths.meeting_dir(&id).join(file);
    blocking(move || clip_data_url(&path, start_ms, end_ms).map_err(|_| "Couldn't read that recording.".to_string())).await
}

#[tauri::command]
pub fn system_audio_permission() -> String {
    #[cfg(windows)]
    {
        "granted".into()
    }
    #[cfg(target_os = "macos")]
    {
        crate::meetings::capture::mac_tap::system_audio_permission().into()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        "unknown".into()
    }
}

#[tauri::command]
pub fn open_system_audio_settings() {
    #[cfg(target_os = "macos")]
    crate::meetings::capture::mac_tap::open_system_audio_settings();
}

/// Write (or rewrite) the notes of one length. Without `regenerate`, an existing note is kept and
/// nothing is sent. Returns whether a run started.
#[tauri::command]
pub fn meeting_enhance(app: AppHandle, state: State<'_, AppState>, id: String, length: Length, regenerate: bool) -> Result<bool, String> {
    valid_id(&id)?;
    if !regenerate && state.store.notes(&id).map_err(|e| e.to_string())?.iter().any(|n| n.length == length) {
        return Ok(false);
    }
    Ok(spawn_enhance(&app, id, length))
}

/// Lengths being written for a meeting right now (so a reopened page shows "Writing…").
#[tauri::command]
pub fn meeting_generating(id: String) -> Vec<Length> {
    GENERATING.lock().iter().filter(|(i, _)| *i == id).map(|(_, l)| *l).collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    pub saved: bool,
}

#[tauri::command]
pub async fn claude_key_status() -> Result<KeyStatus, String> {
    blocking(|| Ok(KeyStatus { saved: crate::meetings::claude::KeyStore::system().get().is_some() })).await
}

/// Check the pasted key with a tiny request, then save it. The key is never returned to the page.
#[tauri::command]
pub async fn claude_key_save(key: String) -> Result<(), String> {
    use crate::meetings::claude::{looks_like_key, test_key, ClaudeError, KeyStore};
    if !looks_like_key(&key) {
        return Err("That doesn't look like a Claude API key (they start with sk-ant-).".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        match test_key(&key) {
            Ok(()) => {}
            // Busy or rate-limited still proves the key was accepted.
            Err(ClaudeError::RateLimited | ClaudeError::Overloaded) => {}
            Err(ClaudeError::Unauthorized) => return Err("Claude rejected that key. Check it and try again.".to_string()),
            Err(e) => return Err(e.to_string()),
        }
        KeyStore::system().set(&key)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn claude_key_remove() -> Result<(), String> {
    blocking(|| crate::meetings::claude::KeyStore::system().remove()).await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spend {
    pub usd: f64,
    pub notes: u32,
}

/// Estimated spend on enhanced notes since the 1st of this month (local time).
#[tauri::command]
pub fn claude_spend_month(state: State<'_, AppState>) -> Result<Spend, String> {
    use chrono::{Datelike, Local, TimeZone};
    let now = Local::now();
    let first = Local
        .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
        .earliest()
        .map(|t| t.with_timezone(&chrono::Utc))
        .unwrap_or_else(chrono::Utc::now);
    let (usd, notes) = state.store.notes_cost_since(first).map_err(|e| e.to_string())?;
    Ok(Spend { usd, notes })
}

/// Tray: pause/resume or stop the meeting (never on the main thread; stop can take a while).
pub fn tray_action(app: &AppHandle, action: &str) {
    let Some(m) = app.try_state::<Meetings>() else { return };
    let s = m.0.clone();
    let action = action.to_string();
    std::thread::spawn(move || match action.as_str() {
        "meeting_pause" => match s.active().map(|a| a.status) {
            Some(MeetingStatus::Paused) => {
                let _ = s.resume();
            }
            Some(_) => {
                let _ = s.pause();
            }
            None => {}
        },
        "meeting_stop" => {
            let _ = s.stop();
        }
        _ => {}
    });
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
