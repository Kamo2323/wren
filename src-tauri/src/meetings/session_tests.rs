//! MeetingRecorder tests (plan Task 7): fake sources the test feeds by hand, a fake transcriber that
//! reads a "marker" from each clip's peak level, and a real Store in a temp folder.

use super::*;
use crate::meetings::speakers::TeamsObservation;
use crate::meetings::{CaptureMode, MeetingStatus, Source};
use crate::store::Store;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const SR: u64 = 16_000;

/// Text the fake transcriber returns for a clip whose peak is about `amp`.
fn text_for(amp: f32) -> &'static str {
    match (amp * 20.0).round() as i32 {
        2 => "hello from me",               // 0.10
        4 => "the rollout moves to thursday", // 0.20
        6 => "tom will chase qa today",     // 0.30
        8 => "fine by me as long as qa signs off", // 0.40
        _ => "something else",
    }
}

struct FakeTranscriber {
    delay: Duration,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    calls: AtomicUsize,
}

impl FakeTranscriber {
    fn new(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            delay: Duration::from_millis(delay_ms),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        })
    }
}

impl Transcriber for FakeTranscriber {
    fn transcribe(&self, wav: &Path, _clip_ms: u64, _bias: &[String]) -> Result<String, String> {
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        let peak = hound::WavReader::open(wav)
            .map_err(|e| e.to_string())?
            .samples::<i16>()
            .map(|s| (s.unwrap() as f32 / i16::MAX as f32).abs())
            .fold(0.0f32, f32::max);
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(text_for(peak).to_string())
    }
}

/// A source whose sender the test drives.
#[derive(Clone, Default)]
struct Manual {
    tx: Arc<Mutex<Option<crossbeam_channel::Sender<AudioBlock>>>>,
}
impl Manual {
    fn send(&self, samples: Vec<f32>, pos_ms: u64) {
        let tx = self.tx.lock().clone().expect("source started");
        tx.send(AudioBlock { samples, stream_pos_ms: pos_ms }).unwrap();
    }
    /// Silence until `from`, a tone of `amp` for `ms`, then 1 s of silence — in 100 ms blocks.
    fn utter(&self, from: u64, ms: u64, amp: f32) {
        let tone: Vec<f32> = (0..(SR * ms / 1000) as usize)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / SR as f32).sin())
            .collect();
        let mut all = tone;
        all.extend(vec![0.0; SR as usize]);
        let mut pos = from;
        for chunk in all.chunks((SR / 10) as usize) {
            self.send(chunk.to_vec(), pos);
            pos += chunk.len() as u64 * 1000 / SR;
        }
    }
    fn silence(&self, from: u64, ms: u64) {
        let mut pos = from;
        for _ in 0..ms / 100 {
            self.send(vec![0.0; (SR / 10) as usize], pos);
            pos += 100;
        }
    }
}
struct ManualSource(Manual, CaptureMode);
impl AudioSource for ManualSource {
    fn start(
        &mut self,
        tx: crossbeam_channel::Sender<AudioBlock>,
        _mode: crossbeam_channel::Sender<CaptureMode>,
    ) -> anyhow::Result<CaptureMode> {
        *self.0.tx.lock() = Some(tx);
        Ok(self.1)
    }
    fn stop(&mut self) {
        self.0.tx.lock().take();
    }
}

struct Rig {
    _dir: TempDir,
    store: Arc<Store>,
    svc: MeetingService,
    me: Manual,
    them: Manual,
    fake: Arc<FakeTranscriber>,
    events: Arc<Mutex<Vec<MeetingEvent>>>,
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rig_with(delay_ms: u64, drain: Duration) -> Rig {
    let dir = std::env::temp_dir().join(format!("wren-meet-svc-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::open(&dir.join("wren.db")).unwrap());
    let fake = FakeTranscriber::new(delay_ms);
    let (me, them) = (Manual::default(), Manual::default());
    let (m2, t2) = (me.clone(), them.clone());
    let deps = Deps {
        store: store.clone(),
        transcriber: fake.clone(),
        meetings_dir: dir.join("meetings"),
        make_sources: Arc::new(move || {
            (
                Box::new(ManualSource(m2.clone(), CaptureMode::TeamsOnly)) as Box<dyn AudioSource>,
                Box::new(ManualSource(t2.clone(), CaptureMode::TeamsOnly)) as Box<dyn AudioSource>,
            )
        }),
        make_teams_reader: None,
        bias_terms: Arc::new(Vec::new),
        me_name: Arc::new(|| "Maximilian".to_string()),
        drain_timeout: drain,
        max_backlog_ms: 600_000,
    };
    let svc = MeetingService::new(deps);
    let events = Arc::new(Mutex::new(Vec::new()));
    let ev = events.clone();
    svc.set_listener(Arc::new(move |e| ev.lock().push(e)));
    Rig { _dir: TempDir(dir), store, svc, me, them, fake, events }
}

fn rig() -> Rig {
    rig_with(20, Duration::from_secs(10))
}

fn wait_for(what: &str, timeout_ms: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn obs(at: u64, speaking: &[&str], muted: Option<bool>) -> TeamsObservation {
    TeamsObservation {
        at_ms: at,
        speaking: speaking.iter().map(|s| s.to_string()).collect(),
        roster: vec!["Sarah Jones".into(), "Max Teams (You)".into()],
        self_name: Some("Max Teams".into()),
        self_muted: muted,
        title: Some("Weekly sync".into()),
    }
}

fn observe_span(svc: &MeetingService, from: u64, to: u64, speaking: &[&str], muted: Option<bool>) {
    let mut at = from;
    while at < to {
        svc.observe(obs(at, speaking, muted));
        at += 250;
    }
}

#[test]
fn two_streams_produce_ordered_named_segments() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    observe_span(&r.svc, 0, 2_900, &[], None);
    observe_span(&r.svc, 2_900, 4_400, &["Sarah Jones"], None);
    observe_span(&r.svc, 4_400, 6_000, &[], None);
    r.me.utter(0, 1_000, 0.10);
    r.them.silence(0, 2_000);
    r.them.utter(2_000, 1_000, 0.20);
    wait_for("two segments", 5_000, || r.store.segments(&m.id).unwrap().len() == 2);
    let segs = r.store.segments(&m.id).unwrap();
    assert_eq!(segs[0].source, Source::Me);
    assert_eq!(segs[0].speaker.as_deref(), Some("Maximilian"));
    assert_eq!(segs[0].text, "hello from me");
    assert_eq!(segs[1].source, Source::Them);
    assert_eq!(segs[1].speaker.as_deref(), Some("Sarah Jones"));
    assert_eq!(segs[1].text, "the rollout moves to thursday");
    // Watcher facts reach the meeting row: participants without the user, the Teams title.
    let row = r.store.meeting_get(&m.id).unwrap().unwrap();
    assert_eq!(row.participants, vec!["Sarah Jones".to_string()]);
    assert_eq!(row.teams_self_name.as_deref(), Some("Max Teams"));
    assert_eq!(row.title, "Weekly sync");
    assert!(r.events.lock().iter().any(|e| matches!(e, MeetingEvent::Segment { .. })));
    r.svc.stop();
}

#[test]
fn one_request_in_flight_max() {
    let r = rig_with(60, Duration::from_secs(10));
    let m = r.svc.start(None).unwrap();
    for i in 0..4 {
        r.me.utter(i * 2_000, 800, 0.10);
        r.them.utter(i * 2_000, 800, 0.30);
    }
    wait_for("eight segments", 8_000, || r.fake.calls.load(Ordering::SeqCst) == 8);
    r.svc.stop();
    assert_eq!(r.fake.max_in_flight.load(Ordering::SeqCst), 1);
    // Me and Them overlap in time but say different things: both kept.
    assert_eq!(r.store.segments(&m.id).unwrap().len(), 8);
}

#[test]
fn pause_excludes_time_and_audio() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 1_000, 0.10);
    wait_for("first segment", 3_000, || r.store.segments(&m.id).unwrap().len() == 1);
    r.svc.pause().unwrap();
    assert_eq!(r.svc.active().unwrap().status, MeetingStatus::Paused);
    r.me.utter(2_000, 1_000, 0.20); // spoken while paused: never transcribed
    std::thread::sleep(Duration::from_millis(400));
    let paused_elapsed = r.svc.active().unwrap().elapsed_ms;
    std::thread::sleep(Duration::from_millis(300));
    assert!(r.svc.active().unwrap().elapsed_ms - paused_elapsed < 50, "clock stops while paused");
    r.svc.resume().unwrap();
    r.me.utter(4_000, 1_000, 0.30);
    wait_for("second segment", 3_000, || r.store.segments(&m.id).unwrap().len() == 2);
    r.svc.stop();
    let texts: Vec<_> = r.store.segments(&m.id).unwrap().into_iter().map(|s| s.text).collect();
    assert_eq!(texts, vec!["hello from me", "tom will chase qa today"]);
    // me.wav keeps the timeline (paused audio is written as silence).
    let wav = hound::WavReader::open(r.svc_dir(&m.id).join("me.wav")).unwrap();
    let secs = wav.duration() as f64 / 16_000.0;
    assert!((secs - 6.0).abs() < 0.15, "{secs}");
    let peak_paused = wav
        .into_samples::<i16>()
        .skip(2_000 * 16)
        .take(1_000 * 16)
        .map(|s| s.unwrap().unsigned_abs())
        .max()
        .unwrap();
    assert_eq!(peak_paused, 0, "no paused audio kept");
}

#[test]
fn dictation_window_drops_me_segment() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    r.svc.dictation_began_at_ms(900);
    r.me.utter(1_000, 1_000, 0.10);
    r.svc.dictation_ended_at_ms(2_100);
    r.me.utter(5_000, 1_000, 0.20);
    wait_for("one segment", 3_000, || r.store.segments(&m.id).unwrap().len() == 1);
    std::thread::sleep(Duration::from_millis(200));
    r.svc.stop();
    let segs = r.store.segments(&m.id).unwrap();
    assert_eq!(segs.len(), 1);
    assert_eq!(segs[0].text, "the rollout moves to thursday");
}

#[test]
fn bleed_me_segment_dropped() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    // The mic picks up the speakers: same words on both streams at the same time.
    r.them.utter(1_000, 1_200, 0.20);
    r.me.utter(1_050, 1_200, 0.20);
    wait_for("calls", 3_000, || r.fake.calls.load(Ordering::SeqCst) == 2);
    r.svc.stop();
    let segs = r.store.segments(&m.id).unwrap();
    assert_eq!(segs.len(), 1, "{segs:?}");
    assert_eq!(segs[0].source, Source::Them);
}

#[test]
fn teams_echo_of_my_line_dropped() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    // Teams hands my own voice back ~0.4 s late (real Mac call): same words on Them, later.
    r.me.utter(1_000, 1_200, 0.20);
    r.them.utter(1_400, 1_200, 0.20);
    wait_for("calls", 3_000, || r.fake.calls.load(Ordering::SeqCst) == 2);
    r.svc.stop();
    let segs = r.store.segments(&m.id).unwrap();
    assert_eq!(segs.len(), 1, "{segs:?}");
    assert_eq!(segs[0].source, Source::Me);
}

#[test]
fn teams_echo_dropped_even_when_its_clip_is_queued_first() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    r.them.utter(1_400, 1_200, 0.20);
    r.me.utter(1_000, 1_200, 0.20);
    wait_for("calls", 3_000, || r.fake.calls.load(Ordering::SeqCst) == 2);
    r.svc.stop();
    let segs = r.store.segments(&m.id).unwrap();
    assert_eq!(segs.len(), 1, "{segs:?}");
    assert_eq!(segs[0].source, Source::Me);
}

#[test]
fn reply_with_other_words_after_me_is_kept() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    r.me.utter(1_000, 1_200, 0.20);
    r.them.utter(1_400, 1_200, 0.30);
    wait_for("two segments", 3_000, || r.store.segments(&m.id).unwrap().len() == 2);
    r.svc.stop();
    assert_eq!(r.store.segments(&m.id).unwrap().len(), 2);
}

#[test]
fn muted_me_segment_flagged() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    observe_span(&r.svc, 0, 3_000, &[], Some(true));
    observe_span(&r.svc, 3_000, 8_000, &[], Some(false));
    r.me.utter(500, 1_000, 0.10);
    r.me.utter(4_000, 1_000, 0.20);
    wait_for("two segments", 3_000, || r.store.segments(&m.id).unwrap().len() == 2);
    r.svc.stop();
    let segs = r.store.segments(&m.id).unwrap();
    assert!(segs[0].muted);
    assert!(!segs[1].muted);
}

#[test]
fn backlog_sets_catching_up() {
    let r = rig_with(1_500, Duration::from_millis(200));
    r.svc.start(None).unwrap();
    // 80 s of continuous speech → four 20 s segments queued behind a slow transcriber.
    r.me.utter(0, 80_000, 0.10);
    wait_for("catching up", 3_000, || r.svc.active().unwrap().catching_up);
    r.svc.stop();
}

#[test]
fn stop_drains_then_marks_untranscribed() {
    let r = rig_with(800, Duration::from_millis(300));
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 1_000, 0.10);
    r.me.utter(3_000, 1_000, 0.20);
    r.me.utter(6_000, 1_000, 0.30);
    std::thread::sleep(Duration::from_millis(100));
    let t = Instant::now();
    let id = r.svc.stop().unwrap();
    assert_eq!(id, m.id);
    assert!(t.elapsed() < Duration::from_millis(1_500), "stop doesn't wait for the slow queue");
    let segs = r.store.segments(&m.id).unwrap();
    assert_eq!(segs.len(), 3, "{segs:?}");
    assert!(segs.iter().any(|s| s.text == NOT_TRANSCRIBED));
    let row = r.store.meeting_get(&m.id).unwrap().unwrap();
    assert_eq!(row.status, MeetingStatus::Ended);
    assert!(row.ended_at_utc.is_some());
    assert!(r.svc.active().is_none());
    // A late result from the in-flight clip must not add a duplicate row.
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(r.store.segments(&m.id).unwrap().len(), 3);
}

#[test]
fn second_start_rejected() {
    let r = rig();
    r.svc.start(None).unwrap();
    assert!(r.svc.start(None).is_err());
    r.svc.stop();
    assert!(r.svc.start(Some("Next".into())).is_ok());
    r.svc.stop();
}

#[test]
fn drop_finalises_wavs() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 1_000, 0.10);
    std::thread::sleep(Duration::from_millis(200));
    let dir = r.svc_dir(&m.id);
    drop(r.svc);
    let wav = hound::WavReader::open(dir.join("me.wav")).expect("valid header after drop");
    assert!(wav.duration() >= 16_000);
}

impl Rig {
    fn svc_dir(&self, id: &str) -> PathBuf {
        self.svc.meeting_dir(id)
    }
}

// ---------------- council round 1 ----------------

struct SlowStart(Manual, Duration);
impl AudioSource for SlowStart {
    fn start(
        &mut self,
        tx: crossbeam_channel::Sender<AudioBlock>,
        _mode: crossbeam_channel::Sender<CaptureMode>,
    ) -> anyhow::Result<CaptureMode> {
        std::thread::sleep(self.1);
        *self.0.tx.lock() = Some(tx);
        Ok(CaptureMode::TeamsOnly)
    }
    fn stop(&mut self) {
        self.0.tx.lock().take();
    }
}

#[test]
fn many_queued_clips_keep_sequence_numbers_small() {
    // Sequence numbers used to add the last two together (Fibonacci) and overflowed after ~90 clips.
    let r = rig_with(15, Duration::from_secs(30));
    let m = r.svc.start(None).unwrap();
    for i in 0..120u64 {
        r.me.utter(i * 1_500, 450, 0.10);
    }
    wait_for("120 clips", 20_000, || r.store.segments(&m.id).unwrap().len() == 120);
    r.svc.stop();
}

#[test]
fn quitting_stop_skips_the_notes_hook() {
    let r = rig();
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    let hook: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |_| {
        f.fetch_add(1, Ordering::SeqCst);
    });
    r.svc.set_on_stopped(hook.clone());
    r.svc.start(None).unwrap();
    r.svc.stop_for_quit(Duration::from_millis(200));
    assert_eq!(fired.load(Ordering::SeqCst), 0, "no paid request on quit");
    let next_launch = rig();
    next_launch.svc.set_on_stopped(hook);
    next_launch.svc.start(None).unwrap();
    next_launch.svc.stop();
    assert_eq!(fired.load(Ordering::SeqCst), 1);
}

#[test]
fn slow_capture_start_does_not_block_dictation_notices() {
    let dir = std::env::temp_dir().join(format!("wren-meet-slow-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::open(&dir.join("wren.db")).unwrap());
    let (me, them) = (Manual::default(), Manual::default());
    let deps = Deps {
        store,
        transcriber: FakeTranscriber::new(10),
        meetings_dir: dir.join("meetings"),
        make_sources: Arc::new(move || {
            (
                Box::new(ManualSource(me.clone(), CaptureMode::TeamsOnly)) as Box<dyn AudioSource>,
                Box::new(SlowStart(them.clone(), Duration::from_millis(1_500))) as Box<dyn AudioSource>,
            )
        }),
        make_teams_reader: None,
        bias_terms: Arc::new(Vec::new),
        me_name: Arc::new(|| "Maximilian".to_string()),
        drain_timeout: Duration::from_secs(5),
        max_backlog_ms: 600_000,
    };
    let svc = Arc::new(MeetingService::new(deps));
    let s2 = svc.clone();
    let starting = std::thread::spawn(move || s2.start(None).unwrap());
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    svc.dictation_began(Instant::now());
    assert!(t.elapsed() < Duration::from_millis(200), "dictation waited {} ms on a starting meeting", t.elapsed().as_millis());
    starting.join().unwrap();
    assert!(svc.start(None).is_err(), "still one meeting at a time");
    svc.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn backlog_is_capped_and_marked_not_transcribed() {
    let mut r = rig_with(2_000, Duration::from_millis(100));
    r.svc.set_max_backlog_ms(60_000);
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 200_000, 0.10); // ten 20 s clips behind a 2 s-per-clip transcriber
    std::thread::sleep(Duration::from_millis(300));
    assert!(r.svc.backlog_ms_for_test() <= 60_000 + 20_000, "{}", r.svc.backlog_ms_for_test());
    let dropped = r.store.segments(&m.id).unwrap().into_iter().filter(|s| s.text == NOT_TRANSCRIBED).count();
    assert!(dropped >= 5, "oldest clips saved as gaps, not silently lost ({dropped})");
    r.svc.stop();
    let _ = &mut r;
}

#[test]
fn failed_start_leaves_no_recording_row() {
    let r = rig();
    // A file where the meetings folder should be: the WAVs can't be created.
    let blocker = r.svc.meeting_dir("x").parent().unwrap().to_path_buf();
    std::fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    std::fs::write(&blocker, b"not a folder").unwrap();
    assert!(r.svc.start(None).is_err());
    let rows = r.store.meeting_list().unwrap();
    assert!(rows.iter().all(|m| m.status != MeetingStatus::Recording), "{rows:?}");
    assert!(r.svc.active().is_none());
}

#[test]
fn own_teams_name_saved_even_when_roster_unchanged() {
    let r = rig();
    let m = r.svc.start(None).unwrap();
    let mut o = obs(0, &[], None);
    o.self_name = None;
    o.roster = vec!["Sarah Jones".into()];
    r.svc.observe(o);
    let mut o = obs(250, &[], None);
    o.roster = vec!["Sarah Jones".into()]; // same people, but now Teams shows the user's own tile
    r.svc.observe(o);
    wait_for("self name", 2_000, || r.store.meeting_get(&m.id).unwrap().unwrap().teams_self_name.is_some());
    r.svc.stop();
}

#[test]
fn meeting_counts_as_busy_while_stop_drains() {
    let r = rig_with(700, Duration::from_secs(3));
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 800, 0.10);
    std::thread::sleep(Duration::from_millis(100));
    let svc = Arc::new(r.svc);
    let s2 = svc.clone();
    let stopping = std::thread::spawn(move || s2.stop());
    std::thread::sleep(Duration::from_millis(100));
    assert!(svc.is_busy(&m.id), "delete must wait while stop drains");
    stopping.join().unwrap();
    assert!(!svc.is_busy(&m.id));
}

// ---------------- council round 2 ----------------

#[test]
fn a_new_meeting_cannot_start_while_the_last_one_is_still_stopping() {
    let r = rig_with(700, Duration::from_secs(3));
    r.svc.start(None).unwrap();
    r.me.utter(0, 800, 0.10);
    std::thread::sleep(Duration::from_millis(100));
    let svc = Arc::new(r.svc);
    let s2 = svc.clone();
    let stopping = std::thread::spawn(move || s2.stop());
    std::thread::sleep(Duration::from_millis(80));
    assert!(svc.start(None).is_err(), "one meeting at a time, including while stopping");
    stopping.join().unwrap();
    assert!(svc.start(None).is_ok());
    svc.stop();
}

#[test]
fn quitting_during_a_stop_waits_for_it() {
    let r = rig_with(600, Duration::from_secs(3));
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 800, 0.10);
    std::thread::sleep(Duration::from_millis(100));
    let svc = Arc::new(r.svc);
    let s2 = svc.clone();
    let stopping = std::thread::spawn(move || s2.stop());
    std::thread::sleep(Duration::from_millis(80));
    svc.stop_for_quit(Duration::from_secs(3));
    // When quit returns, the first stop has finished: the meeting is saved as ended.
    assert_eq!(r.store.meeting_get(&m.id).unwrap().unwrap().status, MeetingStatus::Ended);
    stopping.join().unwrap();
}

#[test]
fn dictation_while_the_meeting_is_starting_stays_out() {
    let dir = std::env::temp_dir().join(format!("wren-meet-dstart-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::open(&dir.join("wren.db")).unwrap());
    let (me, them) = (Manual::default(), Manual::default());
    let (m2, t2) = (me.clone(), them.clone());
    let deps = Deps {
        store: store.clone(),
        transcriber: FakeTranscriber::new(10),
        meetings_dir: dir.join("meetings"),
        make_sources: Arc::new(move || {
            (
                Box::new(ManualSource(m2.clone(), CaptureMode::TeamsOnly)) as Box<dyn AudioSource>,
                Box::new(SlowStart(t2.clone(), Duration::from_millis(800))) as Box<dyn AudioSource>,
            )
        }),
        make_teams_reader: None,
        bias_terms: Arc::new(Vec::new),
        me_name: Arc::new(|| "Maximilian".to_string()),
        drain_timeout: Duration::from_secs(5),
        max_backlog_ms: 600_000,
    };
    let svc = Arc::new(MeetingService::new(deps));
    let s2 = svc.clone();
    let starting = std::thread::spawn(move || s2.start(None).unwrap());
    std::thread::sleep(Duration::from_millis(100));
    svc.dictation_began(Instant::now()); // hotkey held while Teams is still attaching
    let m = starting.join().unwrap();
    me.utter(0, 1_000, 0.10); // what the mic heard during that dictation
    svc.dictation_ended(Instant::now());
    me.utter(5_000, 1_000, 0.20);
    wait_for("one segment", 3_000, || store.segments(&m.id).unwrap().len() == 1);
    std::thread::sleep(Duration::from_millis(200));
    svc.stop();
    let segs = store.segments(&m.id).unwrap();
    assert_eq!(segs.len(), 1, "{segs:?}");
    assert_eq!(segs[0].text, "the rollout moves to thursday");
}

// ---------------- council round 3 ----------------

/// A Teams reader that takes `read` per snapshot and never sees a meeting window; flags its drop.
struct SlowReader(Duration, Arc<AtomicBool>);
impl crate::meetings::teams::TeamsReader for SlowReader {
    fn snapshot(&mut self) -> Option<(crate::meetings::teams::snapshot::UiNode, String)> {
        std::thread::sleep(self.0);
        None
    }
}
impl Drop for SlowReader {
    fn drop(&mut self) {
        self.1.store(true, Ordering::SeqCst);
    }
}

fn rig_with_reader(read: Duration, dropped: Arc<AtomicBool>) -> Rig {
    let mut r = rig();
    r.svc.deps.make_teams_reader = Some(Arc::new(move || {
        let dropped = dropped.clone();
        Box::new(move || Some(Box::new(SlowReader(read, dropped)) as Box<dyn crate::meetings::teams::TeamsReader>))
            as crate::meetings::teams::ReaderFactory
    }));
    r
}

#[test]
fn words_before_the_first_teams_mute_read_count_as_muted() {
    let r = rig_with_reader(Duration::from_millis(800), Arc::new(AtomicBool::new(false)));
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 1_000, 0.10); // spoken before Wren has read Teams once
    std::thread::sleep(Duration::from_millis(300));
    r.svc.observe(obs(900, &[], Some(true))); // the first read that shows the mute state: muted
    wait_for("one segment", 5_000, || r.store.segments(&m.id).unwrap().len() == 1);
    r.svc.stop();
    let segs = r.store.segments(&m.id).unwrap();
    assert!(segs[0].muted, "unknown mute state before the first read fails closed: {segs:?}");
}

#[test]
fn empty_watcher_reads_do_not_release_a_clip_before_a_known_mute_read() {
    let r = rig_with_reader(Duration::ZERO, Arc::new(AtomicBool::new(false)));
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 600, 0.10);
    let shared = r.svc.running.lock().as_ref().unwrap().shared.clone();
    wait_for("empty Teams reads past the clip", 2_400, || {
        shared.timeline.lock().latest_at().is_some_and(|at| at >= 1_500)
    });
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(r.fake.calls.load(Ordering::SeqCst), 0, "empty reads must not release the clip");
    r.svc.observe(obs(2_000, &[], Some(false)));
    wait_for("clip saved after the first known read", 1_000, || {
        r.store.segments(&m.id).unwrap().first().is_some_and(|s| s.muted)
    });
    r.svc.stop();
}

#[test]
fn late_first_mute_read_corrects_saved_lines_and_emits_the_update() {
    let r = rig_with_reader(Duration::ZERO, Arc::new(AtomicBool::new(false)));
    let m = r.svc.start(None).unwrap();
    r.me.utter(0, 800, 0.10);
    r.me.utter(10_000, 800, 0.40);
    r.them.utter(6_000, 800, 0.20);
    wait_for("clips saved after the mute wait times out", 8_000, || {
        r.store.segments(&m.id).unwrap().len() == 3
    });
    let before = r.store.segments(&m.id).unwrap();
    let corrected_id = before[2].id;
    assert!(!before[2].muted);
    r.svc.observe(obs(35_000, &[], Some(false)));
    wait_for("replacement segment event", 1_000, || {
        r.events.lock().iter().any(|e| matches!(e,
            MeetingEvent::Segment { segment } if segment.id == corrected_id && segment.muted))
    });
    let after = r.store.segments(&m.id).unwrap();
    assert_eq!(after.iter().map(|s| s.muted).collect::<Vec<_>>(), [false, false, true]);
    assert_eq!(after[2].id, corrected_id);
    let prompt = crate::meetings::enhance::context_block(&m, &after, "", "Maximilian");
    assert!(!prompt.contains(&after[2].text), "reconciled speech stays out of enhanced notes");
    r.svc.stop();
}

#[test]
fn stop_waits_for_the_teams_reader_to_finish() {
    let dropped = Arc::new(AtomicBool::new(false));
    let r = rig_with_reader(Duration::from_millis(600), dropped.clone());
    r.svc.start(None).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    r.svc.stop();
    assert!(dropped.load(Ordering::SeqCst), "the old reader is gone before a new meeting can start");
}

#[test]
fn quitting_while_a_meeting_is_starting_saves_it() {
    let dir = std::env::temp_dir().join(format!("wren-meet-qstart-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let store = Arc::new(Store::open(&dir.join("wren.db")).unwrap());
    let (me, them) = (Manual::default(), Manual::default());
    let deps = Deps {
        store: store.clone(),
        transcriber: FakeTranscriber::new(10),
        meetings_dir: dir.join("meetings"),
        make_sources: Arc::new(move || {
            (
                Box::new(ManualSource(me.clone(), CaptureMode::TeamsOnly)) as Box<dyn AudioSource>,
                Box::new(SlowStart(them.clone(), Duration::from_millis(600))) as Box<dyn AudioSource>,
            )
        }),
        make_teams_reader: None,
        bias_terms: Arc::new(Vec::new),
        me_name: Arc::new(|| "Maximilian".to_string()),
        drain_timeout: Duration::from_secs(5),
        max_backlog_ms: 600_000,
    };
    let svc = Arc::new(MeetingService::new(deps));
    let events = Arc::new(Mutex::new(Vec::new()));
    let received = events.clone();
    svc.set_listener(Arc::new(move |e| received.lock().push(e)));
    let s2 = svc.clone();
    let starting = std::thread::spawn(move || s2.start(None).unwrap());
    std::thread::sleep(Duration::from_millis(100));
    svc.stop_for_quit(Duration::from_secs(1));
    let m = starting.join().unwrap();
    assert!(svc.active().is_none(), "quit stopped the meeting that was starting");
    assert!(!events.lock().iter().any(|e| matches!(e, MeetingEvent::Status { active: Some(_) })));
    assert_eq!(store.meeting_get(&m.id).unwrap().unwrap().status, MeetingStatus::Ended);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn quit_timeout_cannot_publish_a_late_start() {
    let mut r = rig();
    let make_sources = r.svc.deps.make_sources.clone();
    let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
    let (release_tx, release_rx) = crossbeam_channel::bounded(0);
    r.svc.deps.make_sources = Arc::new(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        make_sources()
    });
    let svc = Arc::new(r.svc);
    let starting_svc = svc.clone();
    let starting = std::thread::spawn(move || starting_svc.start(None));
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(svc.stop_for_quit(Duration::ZERO), None);
    release_tx.send(()).unwrap();
    let m = starting.join().unwrap().unwrap();
    assert!(svc.active().is_none(), "startup must stay cancelled after quit's wait expires");
    assert_eq!(r.store.meeting_get(&m.id).unwrap().unwrap().status, MeetingStatus::Ended);
    assert!(r.me.tx.lock().is_none() && r.them.tx.lock().is_none(), "both audio sources stopped");
    for file in ["me.wav", "them.wav"] {
        hound::WavReader::open(svc.meeting_dir(&m.id).join(file)).unwrap();
    }
}

#[test]
fn a_start_after_quit_is_rejected() {
    let r = rig();
    assert_eq!(r.svc.stop_for_quit(Duration::ZERO), None);
    assert!(r.svc.start(None).is_err());
    assert!(r.store.meeting_list().unwrap().is_empty());
}

#[test]
fn quitting_cuts_a_long_stop_short_and_still_saves() {
    let r = rig_with(1_500, Duration::from_secs(60));
    let fired = Arc::new(AtomicUsize::new(0));
    let f = fired.clone();
    r.svc.set_on_stopped(Arc::new(move |_| {
        f.fetch_add(1, Ordering::SeqCst);
    }));
    let m = r.svc.start(None).unwrap();
    for i in 0..6u64 {
        r.me.utter(i * 3_000, 1_000, 0.10);
    }
    std::thread::sleep(Duration::from_millis(100));
    let svc = Arc::new(r.svc);
    let s2 = svc.clone();
    let stopping = std::thread::spawn(move || s2.stop());
    std::thread::sleep(Duration::from_millis(100));
    let t = Instant::now();
    svc.stop_for_quit(Duration::from_millis(300));
    assert!(t.elapsed() < Duration::from_secs(4), "quit waited {} ms", t.elapsed().as_millis());
    let row = r.store.meeting_get(&m.id).unwrap().unwrap();
    assert_eq!(row.status, MeetingStatus::Ended);
    assert!(r.store.segments(&m.id).unwrap().iter().any(|s| s.text == NOT_TRANSCRIBED));
    stopping.join().unwrap();
    assert_eq!(fired.load(Ordering::SeqCst), 0, "no paid notes request while quitting");
}
