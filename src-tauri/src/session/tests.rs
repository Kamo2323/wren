//! Session tests: the pure helpers, the popup contract, and the whole flow against fakes (no hook,
//! no mic, no engine, no clipboard). Each test gets its own temp folder and database.

use super::*;
use crate::dictionary::Source;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64};

// ---------- pure helpers and the popup contract ----------

fn json(s: &PopupState) -> String {
    serde_json::to_string(s).unwrap()
}

#[test]
fn popup_state_serialises_exactly_like_the_typescript_union() {
    assert_eq!(json(&PopupState::Hidden), r#"{"kind":"hidden"}"#);
    assert_eq!(json(&PopupState::Listening { cpu: true }), r#"{"kind":"listening","cpu":true}"#);
    assert_eq!(
        json(&PopupState::HandsFree { started_at_ms: 1_700_000_000_000, cpu: false }),
        r#"{"kind":"handsfree","startedAtMs":1700000000000,"cpu":false}"#
    );
    assert_eq!(json(&PopupState::Warming), r#"{"kind":"warming"}"#);
    assert_eq!(json(&PopupState::Transcribing { since_ms: 5 }), r#"{"kind":"transcribing","sinceMs":5}"#);
    assert_eq!(json(&PopupState::Pasted), r#"{"kind":"pasted"}"#);
    assert_eq!(json(&PopupState::Nothing), r#"{"kind":"nothing"}"#);
    assert_eq!(json(&PopupState::NoSound { recording: false }), r#"{"kind":"noSound","recording":false}"#);
    assert_eq!(json(&PopupState::NoSound { recording: true }), r#"{"kind":"noSound","recording":true}"#);
    assert_eq!(json(&PopupState::NotPasted { text: "Hi".into() }), r#"{"kind":"notPasted","text":"Hi"}"#);
    assert_eq!(
        json(&PopupState::Learned { from: "a".into(), to: "b".into(), entry_id: 7 }),
        r#"{"kind":"learned","from":"a","to":"b","entryId":7}"#
    );
    assert_eq!(json(&PopupState::Error { message: "m".into() }), r#"{"kind":"error","message":"m"}"#);
    assert_eq!(
        json(&PopupState::CapWarning { remaining_ms: 60_000, mode: CapMode::Hold }),
        r#"{"kind":"capWarning","remainingMs":60000,"mode":"hold"}"#
    );
    assert_eq!(
        json(&PopupState::CapWarning { remaining_ms: 1, mode: CapMode::HandsFree }),
        r#"{"kind":"capWarning","remainingMs":1,"mode":"handsFree"}"#
    );
}

#[test]
fn popup_debug_never_shows_dictated_text() {
    let s = format!(
        "{:?} {:?}",
        PopupState::NotPasted { text: "secret words".into() },
        PopupState::Learned { from: "secret".into(), to: "words".into(), entry_id: 1 }
    );
    assert!(!s.contains("secret") && !s.contains("words"), "{s}");
}

#[test]
fn popup_dismiss_times_and_clickable_states_follow_the_contract() {
    use PopupState as P;
    assert_eq!(P::Pasted.dismiss_ms(), Some(POPUP_PASTED_MS));
    assert_eq!(P::Nothing.dismiss_ms(), Some(POPUP_NOTHING_MS));
    assert_eq!(P::NoSound { recording: false }.dismiss_ms(), Some(POPUP_NO_SOUND_MS));
    // M-1: shown during a recording it stays until the recording replaces it.
    assert_eq!(P::NoSound { recording: true }.dismiss_ms(), None);
    assert_eq!(P::NotPasted { text: "x".into() }.dismiss_ms(), Some(POPUP_NOT_PASTED_MS));
    assert_eq!(P::Learned { from: "a".into(), to: "b".into(), entry_id: 1 }.dismiss_ms(), Some(POPUP_LEARNED_MS));
    assert_eq!(P::Error { message: "m".into() }.dismiss_ms(), Some(POPUP_ERROR_MS));
    for s in [
        P::Hidden,
        P::Listening { cpu: false },
        P::HandsFree { started_at_ms: 0, cpu: false },
        P::Warming,
        P::Transcribing { since_ms: 0 },
        P::CapWarning { remaining_ms: 1, mode: CapMode::Hold },
    ] {
        assert_eq!(s.dismiss_ms(), None, "{s:?}");
    }
    // Clickable: notPasted, learned, error, hands-free (×), and a hands-free cap warning (keeps its ×).
    assert!(P::NotPasted { text: "x".into() }.is_clickable());
    assert!(P::Learned { from: "a".into(), to: "b".into(), entry_id: 1 }.is_clickable());
    assert!(P::Error { message: "m".into() }.is_clickable());
    assert!(P::HandsFree { started_at_ms: 0, cpu: false }.is_clickable());
    assert!(P::CapWarning { remaining_ms: 1, mode: CapMode::HandsFree }.is_clickable());
    assert!(P::NoSound { recording: false }.is_clickable(), "opens the microphone privacy settings");
    assert!(P::NoSound { recording: true }.is_clickable());
    for s in [
        P::Hidden,
        P::Listening { cpu: false },
        P::Warming,
        P::Transcribing { since_ms: 0 },
        P::Pasted,
        P::Nothing,
        P::CapWarning { remaining_ms: 1, mode: CapMode::Hold },
    ] {
        assert!(!s.is_clickable(), "{s:?}");
    }
}

#[test]
fn paste_results_map_to_outcomes_and_copy_decisions() {
    assert_eq!(outcome_for(&PasteResult::Pasted), (PasteOutcome::Pasted, false));
    assert_eq!(outcome_for(&PasteResult::FocusChanged), (PasteOutcome::NotPasted, true));
    assert_eq!(outcome_for(&PasteResult::KeysStillHeld), (PasteOutcome::NotPasted, true));
    assert_eq!(outcome_for(&PasteResult::OwnWindow), (PasteOutcome::NotPasted, true));
    assert_eq!(outcome_for(&PasteResult::NotEditable), (PasteOutcome::NotPasted, true));
    // Ctrl+V and Ctrl+Shift+V were sent and nobody read: the text is already on the clipboard.
    assert_eq!(outcome_for(&PasteResult::Failed(REASON_NOT_READ.into())), (PasteOutcome::Copied, false));
    assert_eq!(outcome_for(&PasteResult::Failed("clipboard busy".into())), (PasteOutcome::Failed, true));
}

#[test]
fn term_hits_are_case_insensitive_whole_words() {
    let terms = vec![
        (1, "Zorvath".to_string()),
        (2, "Quillby".to_string()),
        (3, "GardX".to_string()),
        (4, "half past".to_string()),
        (5, "ren".to_string()),
        (6, "".to_string()),
    ];
    assert_eq!(term_hits("Send it to zorvath before Friday.", &terms), vec![1]);
    assert_eq!(term_hits("GARDX and Quillby's report", &terms), vec![2, 3]);
    assert_eq!(term_hits("at Half Past three", &terms), vec![4]);
    // Inside another word does not count ("Wren" contains "ren").
    assert_eq!(term_hits("Wren rennet", &terms), Vec::<i64>::new());
    // Counted once however often it appears.
    assert_eq!(term_hits("ren, ren and ren", &terms), vec![5]);
    assert_eq!(term_hits("", &terms), Vec::<i64>::new());
}

// ---------- fakes ----------

struct FakeRecorder {
    /// The fake foreground window (shared with the env), so stopping can move focus.
    fg: Arc<Mutex<ForegroundInfo>>,
    switch_fg_on_stop: AtomicBool,
    active: Mutex<Option<PathBuf>>,
    elapsed: AtomicU64,
    silence: AtomicU64,
    heard: AtomicBool,
    starts: AtomicUsize,
    cancels: AtomicUsize,
    /// How long `start` takes (a cold or Bluetooth mic can take over a second to open).
    start_delay_ms: AtomicU64,
}

fn edge() -> ForegroundInfo {
    ForegroundInfo { hwnd: 42, pid: 4242, process_name: "msedge.exe".into() }
}

impl FakeRecorder {
    fn new(fg: Arc<Mutex<ForegroundInfo>>) -> Self {
        Self {
            fg,
            switch_fg_on_stop: AtomicBool::new(false),
            active: Mutex::new(None),
            elapsed: AtomicU64::new(3_000),
            silence: AtomicU64::new(0),
            heard: AtomicBool::new(true),
            starts: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            start_delay_ms: AtomicU64::new(0),
        }
    }
}

impl Recorder for FakeRecorder {
    fn start(&self, wav_path: PathBuf) -> anyhow::Result<()> {
        std::thread::sleep(Duration::from_millis(self.start_delay_ms.load(Ordering::SeqCst)));
        let mut a = self.active.lock();
        anyhow::ensure!(a.is_none(), "already recording");
        std::fs::create_dir_all(wav_path.parent().unwrap())?;
        std::fs::write(&wav_path, b"RIFF")?;
        *a = Some(wav_path);
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) -> anyhow::Result<Recording> {
        let p = self.active.lock().take().ok_or_else(|| anyhow::anyhow!("not recording"))?;
        if self.switch_fg_on_stop.load(Ordering::SeqCst) {
            *self.fg.lock() = ForegroundInfo { hwnd: 7, pid: 77, process_name: "other.exe".into() };
        }
        Ok(Recording {
            wav_path: p,
            duration_ms: self.elapsed.load(Ordering::SeqCst),
            heard_sound: self.heard.load(Ordering::SeqCst),
        })
    }
    fn cancel(&self) {
        if let Some(p) = self.active.lock().take() {
            let _ = std::fs::remove_file(p);
            self.cancels.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn silence_ms(&self) -> u64 {
        if self.active.lock().is_some() { self.silence.load(Ordering::SeqCst) } else { 0 }
    }
    fn elapsed_ms(&self) -> u64 {
        if self.active.lock().is_some() { self.elapsed.load(Ordering::SeqCst) } else { 0 }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Ev {
    Popup(PopupState),
    Show,
    Hide,
    Clickable(bool),
    Paste { text: String, saved_first: bool },
    Copy(String),
    History,
    Hotkey(HotkeyCmd),
    Sound(Sound),
    Started,
    PastedHook(String),
    Transcribe { bias: Vec<String> },
}

struct FakeEnv {
    dir: PathBuf,
    store: Store,
    settings: Mutex<Settings>,
    recorder: FakeRecorder,
    status: Mutex<EngineStatus>,
    /// Scripted engine replies (text, delay ms), in call order; empty → "Hello there."
    replies: Mutex<VecDeque<(Result<String, EngineError>, u64)>>,
    paste_results: Mutex<VecDeque<PasteResult>>,
    fg: Arc<Mutex<ForegroundInfo>>,
    paste_at: Mutex<Vec<Instant>>,
    events: Mutex<Vec<Ev>>,
    ai_calls: AtomicUsize,
    /// The pre-paste check's answer (Task 9).
    not_editable: std::sync::atomic::AtomicBool,
}

impl FakeEnv {
    fn new() -> Arc<Self> {
        let dir = std::env::temp_dir().join(format!("wren-session-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("db").join("wren.db")).unwrap();
        let fg = Arc::new(Mutex::new(edge()));
        Arc::new(Self {
            dir,
            store,
            settings: Mutex::new(Settings::default()),
            recorder: FakeRecorder::new(fg.clone()),
            fg,
            paste_at: Mutex::new(Vec::new()),
            status: Mutex::new(EngineStatus::Ready { model: "large-v3-turbo".into(), device: "cuda".into(), vram_mb: 1400 }),
            replies: Mutex::new(VecDeque::new()),
            paste_results: Mutex::new(VecDeque::new()),
            events: Mutex::new(Vec::new()),
            ai_calls: AtomicUsize::new(0),
            not_editable: std::sync::atomic::AtomicBool::new(false),
        })
    }
    fn push(&self, e: Ev) {
        self.events.lock().push(e);
    }
    fn events(&self) -> Vec<Ev> {
        self.events.lock().clone()
    }
    fn popups(&self) -> Vec<PopupState> {
        self.events().into_iter().filter_map(|e| if let Ev::Popup(p) = e { Some(p) } else { None }).collect()
    }
    fn pastes(&self) -> Vec<String> {
        self.events().into_iter().filter_map(|e| if let Ev::Paste { text, .. } = e { Some(text) } else { None }).collect()
    }
    fn reply(&self, text: &str, delay_ms: u64) {
        self.replies.lock().push_back((Ok(text.into()), delay_ms));
    }
    fn rows(&self) -> Vec<crate::store::TranscriptionRow> {
        self.store.recent(100).unwrap()
    }
}

impl Drop for FakeEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Env for FakeEnv {
    fn store(&self) -> &Store {
        &self.store
    }
    fn settings(&self) -> Settings {
        self.settings.lock().clone()
    }
    fn audio_dir(&self) -> PathBuf {
        self.dir.join("audio")
    }
    fn recorder(&self) -> Option<&dyn Recorder> {
        Some(&self.recorder)
    }
    fn engine_status(&self) -> EngineStatus {
        self.status.lock().clone()
    }
    fn transcribe(&self, req: TranscribeRequest, _timeout: Duration) -> Result<TranscribeResult, EngineError> {
        self.push(Ev::Transcribe { bias: req.bias_terms.clone() });
        let (text, delay) = self.replies.lock().pop_front().unwrap_or((Ok("Hello there.".into()), 0));
        std::thread::sleep(Duration::from_millis(delay));
        let text = text?;
        Ok(TranscribeResult { speech_ms: if text.is_empty() { 0 } else { 2_000 }, text, infer_ms: 1, segments: vec![] })
    }
    fn foreground(&self) -> ForegroundInfo {
        self.fg.lock().clone()
    }
    fn paste(&self, text: &str, _target: &ForegroundInfo) -> PasteResult {
        self.paste_at.lock().push(Instant::now());
        let saved_first = self.store.recent(100).unwrap().iter().any(|r| r.final_text == text);
        self.push(Ev::Paste { text: text.into(), saved_first });
        self.paste_results.lock().pop_front().unwrap_or(PasteResult::Pasted)
    }
    fn copy_text(&self, text: &str) -> anyhow::Result<()> {
        self.push(Ev::Copy(text.into()));
        Ok(())
    }
    fn popup_emit(&self, state: &PopupState) {
        self.push(Ev::Popup(state.clone()));
    }
    fn popup_show(&self) {
        self.push(Ev::Show);
    }
    fn popup_hide(&self) {
        self.push(Ev::Hide);
    }
    fn popup_clickable(&self, clickable: bool) {
        self.push(Ev::Clickable(clickable));
    }
    fn history_changed(&self) {
        self.push(Ev::History);
    }
    fn hotkey(&self, cmd: HotkeyCmd) {
        self.push(Ev::Hotkey(cmd));
    }
    fn play(&self, sound: Sound) {
        self.push(Ev::Sound(sound));
    }
    fn dictation_started(&self) {
        self.push(Ev::Started);
    }
    fn pasted(&self, d: &PastedDictation) {
        self.push(Ev::PastedHook(d.text.clone()));
    }
    fn target_not_editable(&self, _target: &ForegroundInfo) -> bool {
        self.not_editable.load(Ordering::SeqCst)
    }
    fn ai_cleanup(&self, tidied: &str) -> Option<String> {
        self.ai_calls.fetch_add(1, Ordering::SeqCst);
        Some(tidied.replace("Hello", "Hello,"))
    }
}

struct Rig {
    env: Arc<FakeEnv>,
    hk: Sender<HotkeyEvent>,
    session: SessionHandle,
}

fn rig() -> Rig {
    let env = FakeEnv::new();
    let (hk, hk_rx) = crossbeam_channel::unbounded();
    let session = start(env.clone(), hk_rx).expect("session starts");
    Rig { env, hk, session }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.session.send(Msg::Shutdown);
    }
}

fn wait_until(what: &str, timeout_ms: u64, cond: impl Fn() -> bool) {
    let t = Instant::now();
    while !cond() {
        assert!(t.elapsed() < Duration::from_millis(timeout_ms), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

// ---------- flows ----------

#[test]
fn hold_saves_to_the_store_before_pasting_then_shows_pasted() {
    let r = rig();
    r.env.store.add_term("Zorvath", Source::Manual).unwrap();
    let term_id = r.env.store.dictionary_list().unwrap()[0].id;
    let rep = r.env.store.add_replacement("Zoro", "Zorvath", Source::Manual, crate::dictionary::Status::Active).unwrap();
    r.env.reply("um send it to zoro and zorvath", 0);

    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("listening popup", 1_000, || r.env.popups().contains(&PopupState::Listening { cpu: false }));
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 3_000 }).unwrap();
    wait_until("paste", 2_000, || !r.env.pastes().is_empty());
    wait_until("pasted popup", 1_000, || r.env.popups().contains(&PopupState::Pasted));

    let ev = r.env.events();
    assert!(ev.contains(&Ev::Started), "dictation_started hook");
    assert!(ev.contains(&Ev::Transcribe { bias: vec!["Zorvath".into()] }), "bias terms from the store: {ev:?}");
    // The pipeline ran (filler gone, replacement applied) and the store had the row before the paste.
    assert!(ev.contains(&Ev::Paste { text: "Send it to Zorvath and zorvath".into(), saved_first: true }), "{ev:?}");
    assert!(ev.contains(&Ev::PastedHook("Send it to Zorvath and zorvath".into())));
    assert!(ev.iter().any(|e| matches!(e, Ev::Popup(PopupState::Transcribing { .. }))));
    assert!(ev.contains(&Ev::History));

    let rows = r.env.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].paste_outcome, PasteOutcome::Pasted);
    assert_eq!(rows[0].mode, Mode::Hold);
    assert_eq!(rows[0].target_process, "msedge.exe");
    // Hits: the applied replacement and the term found in the final text.
    let dict = r.env.store.dictionary_list().unwrap();
    assert_eq!(dict.iter().find(|d| d.id == rep.id).unwrap().hits, 1);
    assert_eq!(dict.iter().find(|d| d.id == term_id).unwrap().hits, 1);
    // The WAV stays for History (the retention sweep removes it).
    assert_eq!(std::fs::read_dir(r.env.audio_dir()).unwrap().count(), 1);
    // After the pasted popup's time the window is hidden and made click-through again.
    sleep(POPUP_PASTED_MS + POPUP_HIDE_AFTER_DISMISS_MS + 150);
    let ev = r.env.events();
    let after_pasted = ev.iter().rposition(|e| *e == Ev::Popup(PopupState::Pasted)).unwrap();
    assert!(ev[after_pasted..].contains(&Ev::Hide), "{ev:?}");
}

#[test]
fn a_quick_tap_is_discarded_and_never_flashes_the_popup() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::Tap).unwrap(); // inside POPUP_DELAY_MS, like Ctrl+Win+→
    wait_until("cancel", 1_000, || r.env.recorder.cancels.load(Ordering::SeqCst) == 1);
    sleep(POPUP_DELAY_MS + 100);
    assert!(r.env.popups().iter().all(|p| *p == PopupState::Hidden), "{:?}", r.env.popups());
    assert!(!r.env.events().contains(&Ev::Show));
    assert!(r.env.rows().is_empty());
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::Transcribe { .. })));
}

#[test]
fn a_tap_after_the_popup_showed_hides_it() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("listening", 1_000, || r.env.popups().contains(&PopupState::Listening { cpu: false }));
    r.hk.send(HotkeyEvent::Tap).unwrap();
    wait_until("hidden", 1_000, || r.env.popups().last() == Some(&PopupState::Hidden));
    assert!(r.env.events().contains(&Ev::Hide));
    assert_eq!(r.env.recorder.cancels.load(Ordering::SeqCst), 1);
}

#[test]
fn cancel_discards_short_holds_and_saves_long_ones_without_pasting() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::Cancel { held_ms: CANCEL_SAVE_AFTER_MS - 1 }).unwrap();
    wait_until("cancel", 1_000, || r.env.recorder.cancels.load(Ordering::SeqCst) == 1);

    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::Cancel { held_ms: CANCEL_SAVE_AFTER_MS }).unwrap();
    wait_until("saved row", 2_000, || r.env.rows().len() == 1);
    sleep(100);
    assert_eq!(r.env.rows()[0].paste_outcome, PasteOutcome::NotPasted);
    assert!(r.env.pastes().is_empty(), "never pasted");
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::Copy(_))), "the clipboard is left alone");
    assert!(!r.env.popups().iter().any(|p| matches!(p, PopupState::NotPasted { .. } | PopupState::Pasted)));
}

#[test]
fn double_tap_runs_hands_free_until_the_next_press() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::Tap).unwrap();
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("hands-free popup", 1_000, || r.env.popups().iter().any(|p| matches!(p, PopupState::HandsFree { .. })));
    let hf = r.env.popups().into_iter().find(|p| matches!(p, PopupState::HandsFree { .. })).unwrap();
    let PopupState::HandsFree { started_at_ms, .. } = hf else { unreachable!() };
    assert!((epoch_ms() - started_at_ms).abs() < 5_000, "epoch milliseconds");
    assert!(r.env.events().contains(&Ev::Clickable(true)), "the × needs clicks");
    r.hk.send(HotkeyEvent::HandsFreeStop).unwrap();
    wait_until("paste", 2_000, || r.env.pastes().len() == 1);
    wait_until("row", 1_000, || r.env.rows().len() == 1);
    assert_eq!(r.env.rows()[0].mode, Mode::HandsFree);
    assert_eq!(r.env.recorder.starts.load(Ordering::SeqCst), 2);
}

#[test]
fn hands_free_stops_on_long_silence_saves_and_does_not_paste() {
    let r = rig();
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    r.env.recorder.silence.store(HANDSFREE_SILENCE_STOP_MS, Ordering::SeqCst);
    wait_until("saved", 3_000, || r.env.rows().len() == 1);
    wait_until("notice", 1_000, || r.env.popups().iter().any(|p| matches!(p, PopupState::NotPasted { .. })));
    assert!(r.env.pastes().is_empty());
    assert!(r.env.events().contains(&Ev::Hotkey(HotkeyCmd::ForceIdle)), "the chord machine forgets hands-free");
    assert_eq!(r.env.rows()[0].paste_outcome, PasteOutcome::NotPasted);
}

#[test]
fn the_cap_warns_then_stops_and_pastes() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    r.env.recorder.elapsed.store(RECORDING_WARN_MS, Ordering::SeqCst);
    wait_until("cap warning", 2_000, || {
        r.env.popups().contains(&PopupState::CapWarning { remaining_ms: MAX_RECORDING_MS - RECORDING_WARN_MS, mode: CapMode::Hold })
    });
    r.env.recorder.elapsed.store(MAX_RECORDING_MS, Ordering::SeqCst);
    wait_until("paste after the cap", 3_000, || r.env.pastes().len() == 1);
    assert!(r.env.events().contains(&Ev::Hotkey(HotkeyCmd::ForceIdle)));
    // The (now ignored) release produces nothing more.
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: MAX_RECORDING_MS + 10 }).unwrap();
    sleep(200);
    assert_eq!(r.env.pastes().len(), 1);
}

#[test]
fn the_popup_cross_discards_hands_free() {
    let r = rig();
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    r.session.send(Msg::Discard);
    wait_until("cancel", 1_000, || r.env.recorder.cancels.load(Ordering::SeqCst) == 1);
    wait_until("hidden", 1_000, || r.env.popups().last() == Some(&PopupState::Hidden));
    assert!(r.env.events().contains(&Ev::Hotkey(HotkeyCmd::ForceIdle)));
    assert!(r.env.events().contains(&Ev::Clickable(false)));
    sleep(100);
    assert!(r.env.rows().is_empty());
}

#[test]
fn not_pasted_copies_the_text_and_offers_click_to_copy() {
    let r = rig();
    r.env.paste_results.lock().push_back(PasteResult::FocusChanged);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("notPasted popup", 2_000, || r.env.popups().contains(&PopupState::NotPasted { text: "Hello there.".into() }));
    assert!(r.env.events().contains(&Ev::Copy("Hello there.".into())));
    assert!(r.env.events().contains(&Ev::Clickable(true)));
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::PastedHook(_))), "no watcher without a paste");
    wait_until("outcome", 1_000, || r.env.rows().first().map(|r| r.paste_outcome) == Some(PasteOutcome::NotPasted));
}

#[test]
fn nothing_editable_in_focus_copies_instead_of_pasting() {
    let r = rig();
    r.env.not_editable.store(true, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("notPasted popup", 2_000, || r.env.popups().contains(&PopupState::NotPasted { text: "Hello there.".into() }));
    assert!(r.env.pastes().is_empty(), "no Ctrl+V into a non-editable target");
    assert!(r.env.events().contains(&Ev::Copy("Hello there.".into())));
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::PastedHook(_))), "no watcher without a paste");
    wait_until("outcome", 1_000, || r.env.rows().first().map(|r| r.paste_outcome) == Some(PasteOutcome::NotPasted));
}

#[test]
fn unread_paste_is_recorded_as_copied() {
    let r = rig();
    r.env.paste_results.lock().push_back(PasteResult::Failed(REASON_NOT_READ.into()));
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("outcome", 2_000, || r.env.rows().first().map(|r| r.paste_outcome) == Some(PasteOutcome::Copied));
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::Copy(_))), "already on the clipboard");
    wait_until("popup", 1_000, || r.env.popups().iter().any(|p| matches!(p, PopupState::NotPasted { .. })));
}

#[test]
fn empty_text_is_never_pasted_or_saved() {
    let r = rig();
    r.env.reply("", 0);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("nothing popup", 2_000, || r.env.popups().contains(&PopupState::Nothing));
    assert!(r.env.pastes().is_empty());
    assert!(r.env.rows().is_empty());
}

#[test]
fn no_sound_at_all_skips_the_engine() {
    let r = rig();
    r.env.recorder.heard.store(false, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("noSound popup", 2_000, || r.env.popups().contains(&PopupState::NoSound { recording: false }));
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::Transcribe { .. })));
    assert!(r.env.rows().is_empty());
}

#[test]
fn a_silent_mic_is_flagged_while_still_recording() {
    let r = rig();
    // Never heard anything: silence so far equals the time recorded.
    r.env.recorder.elapsed.store(NO_SOUND_WARN_MS, Ordering::SeqCst);
    r.env.recorder.silence.store(NO_SOUND_WARN_MS, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("noSound while recording", NO_SOUND_WARN_MS + 1_000, || {
        r.env.popups().contains(&PopupState::NoSound { recording: true })
    });
    assert!(r.env.recorder.active.lock().is_some(), "still recording");
    // Sound arrives (e.g. he unmuted): the popup goes back to listening.
    r.env.recorder.silence.store(0, Ordering::SeqCst);
    wait_until("listening again", 1_000, || r.env.popups().last() == Some(&PopupState::Listening { cpu: false }));
}

#[test]
fn hands_free_returns_from_no_sound_with_its_own_start_time() {
    let r = rig();
    r.env.recorder.elapsed.store(NO_SOUND_WARN_MS, Ordering::SeqCst);
    r.env.recorder.silence.store(NO_SOUND_WARN_MS, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("noSound", NO_SOUND_WARN_MS + 1_000, || r.env.popups().contains(&PopupState::NoSound { recording: true }));
    r.env.recorder.silence.store(0, Ordering::SeqCst);
    wait_until("hands-free again", 1_000, || matches!(r.env.popups().last(), Some(PopupState::HandsFree { .. })));
    let starts: Vec<i64> = r
        .env
        .popups()
        .into_iter()
        .filter_map(|p| if let PopupState::HandsFree { started_at_ms, .. } = p { Some(started_at_ms) } else { None })
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0], starts[1], "the timer keeps counting from the real start");
}

#[test]
fn dictations_paste_in_start_order_even_when_the_first_is_slow() {
    let r = rig();
    r.env.reply("First one.", 400);
    r.env.reply("Second one.", 0);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || r.env.recorder.active.lock().is_none());
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("two pastes", 3_000, || r.env.pastes().len() == 2);
    assert_eq!(r.env.pastes(), vec!["First one.".to_string(), "Second one.".to_string()]);
}

#[test]
fn pressed_while_warming_up_records_and_pastes_once_ready() {
    let r = rig();
    *r.env.status.lock() = EngineStatus::WarmingUp;
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("warming popup", 1_000, || r.env.popups().contains(&PopupState::Warming));
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    sleep(300);
    assert!(r.env.pastes().is_empty(), "waits for the engine");
    *r.env.status.lock() = EngineStatus::Ready { model: "m".into(), device: "cuda".into(), vram_mb: 1 };
    wait_until("paste once ready", 2_000, || r.env.pastes().len() == 1);
}

#[test]
fn ready_too_late_after_release_saves_with_a_notice_instead_of_pasting() {
    let r = rig();
    *r.env.status.lock() = EngineStatus::WarmingUp;
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    sleep(WARMUP_PASTE_GRACE_MS + 200);
    *r.env.status.lock() = EngineStatus::Ready { model: "m".into(), device: "cuda".into(), vram_mb: 1 };
    wait_until("saved", 2_000, || r.env.rows().len() == 1);
    wait_until("notice", 1_000, || r.env.popups().iter().any(|p| matches!(p, PopupState::NotPasted { .. })));
    assert!(r.env.pastes().is_empty());
}

fn transcribe_calls(env: &FakeEnv) -> usize {
    env.events().iter().filter(|e| matches!(e, Ev::Transcribe { .. })).count()
}

#[test]
fn an_engine_error_is_retried_once() {
    let r = rig();
    r.env.replies.lock().push_back((Err(EngineError::Internal("crashed".into())), 0));
    r.env.reply("Second try.", 0);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("paste", 3_000, || r.env.pastes().len() == 1);
    assert_eq!(r.env.pastes(), vec!["Second try.".to_string()]);
    assert_eq!(transcribe_calls(&r.env), 2);
    assert!(!r.env.popups().iter().any(|p| matches!(p, PopupState::Error { .. })));
}

#[test]
fn an_engine_failing_twice_keeps_the_audio_and_saves_without_text() {
    let r = rig();
    r.env.replies.lock().push_back((Err(EngineError::Oom("cuda".into())), 0));
    r.env.replies.lock().push_back((Err(EngineError::Timeout), 0));
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("error popup", 3_000, || {
        r.env.popups().contains(&PopupState::Error { message: NOT_TRANSCRIBED_MESSAGE.into() })
    });
    let rows = r.env.rows();
    assert_eq!(rows.len(), 1, "saved in History");
    assert_eq!(rows[0].paste_outcome, PasteOutcome::Failed);
    assert_eq!(rows[0].final_text, "");
    assert_eq!(std::fs::read_dir(r.env.audio_dir()).unwrap().count(), 1, "the WAV is kept");
    assert!(r.env.pastes().is_empty());
    assert_eq!(transcribe_calls(&r.env), 2);
}

#[test]
fn a_failed_engine_saves_the_dictation_without_text() {
    let r = rig();
    *r.env.status.lock() = EngineStatus::Failed { reason: "The speech engine could not start.".into() };
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("error popup", 2_000, || {
        r.env.popups().contains(&PopupState::Error { message: NOT_TRANSCRIBED_MESSAGE.into() })
    });
    assert_eq!(r.env.rows().len(), 1);
    assert_eq!(r.env.rows()[0].paste_outcome, PasteOutcome::Failed);
    assert_eq!(transcribe_calls(&r.env), 0);
}

// ---------- review round 1: back-to-back dictations, notices, quit ----------

#[test]
fn popup_result_is_posted_and_shown() {
    let r = rig();
    r.session.popup_result(PopupState::Learned { from: "zoro".into(), to: "Zorvath".into(), entry_id: 3 });
    wait_until("learned", 1_000, || {
        r.env.popups().contains(&PopupState::Learned { from: "zoro".into(), to: "Zorvath".into(), entry_id: 3 })
    });
    assert!(r.env.events().contains(&Ev::Clickable(true)));
}

#[test]
fn the_first_dictation_waits_for_a_held_second_one_before_pasting() {
    let r = rig();
    r.env.reply("First one.", 100);
    r.env.reply("Second one.", 0);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || r.env.recorder.active.lock().is_none());
    r.hk.send(HotkeyEvent::HoldStart).unwrap(); // the chord is held again
    sleep(600); // the first is transcribed by now
    assert!(r.env.pastes().is_empty(), "no paste while the chord is held (it would be KeysStillHeld)");
    let released = Instant::now();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 600 }).unwrap();
    wait_until("both pasted", 3_000, || r.env.pastes().len() == 2);
    assert_eq!(r.env.pastes(), vec!["First one.".to_string(), "Second one.".to_string()]);
    assert!(r.env.paste_at.lock()[0] >= released, "the first paste came after the second hold ended");
}

/// I-1: the hold counts from the chord, not from the microphone opening — an earlier dictation
/// must not paste while the next hold's mic is still opening (its chord keys are down).
#[test]
fn the_first_dictation_waits_for_a_hold_whose_mic_is_still_opening() {
    let r = rig();
    r.env.reply("First one.", 100);
    r.env.reply("Second one.", 0);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || {
        r.env.recorder.starts.load(Ordering::SeqCst) == 1 && r.env.recorder.active.lock().is_none()
    });
    r.env.recorder.start_delay_ms.store(800, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HoldStart).unwrap(); // chord down; the mic takes 800 ms to open
    sleep(500); // the first is transcribed by now; the second mic is not open yet
    assert!(r.env.pastes().is_empty(), "pasted while the next hold's mic was opening (chord keys down)");
    wait_until("second recording", 2_000, || r.env.recorder.active.lock().is_some());
    sleep(100);
    assert!(r.env.pastes().is_empty(), "pasted during the second hold");
    let released = Instant::now();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("both pasted", 3_000, || r.env.pastes().len() == 2);
    assert_eq!(r.env.pastes(), vec!["First one.".to_string(), "Second one.".to_string()]);
    assert!(r.env.paste_at.lock()[0] >= released, "the first paste came after the second hold ended");
}

/// A failed mic start clears the hold, so a waiting dictation is not held back.
#[test]
fn a_failed_mic_start_does_not_hold_back_an_earlier_paste() {
    let r = rig();
    r.env.reply("First one.", 300);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || {
        r.env.recorder.starts.load(Ordering::SeqCst) == 1 && r.env.recorder.active.lock().is_none()
    });
    // A second start that fails: the fake refuses a start while one is active.
    *r.env.recorder.active.lock() = Some(std::env::temp_dir().join("wren-held.wav"));
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("first pasted", 2_000, || r.env.pastes().len() == 1);
    *r.env.recorder.active.lock() = None;
}

#[test]
fn a_notice_during_a_hands_free_recording_is_shown_when_it_ends() {
    let r = rig();
    r.env.paste_results.lock().push_back(PasteResult::FocusChanged);
    r.env.reply("First one.", 200);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || r.env.recorder.active.lock().is_none());
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("first paste attempted", 2_000, || r.env.pastes().len() == 1);
    sleep(100);
    let notice = PopupState::NotPasted { text: "First one.".into() };
    assert!(!r.env.popups().contains(&notice), "not over the hands-free popup");
    r.hk.send(HotkeyEvent::HandsFreeStop).unwrap();
    wait_until("notice after the recording", 1_000, || r.env.popups().contains(&notice));
    let p = r.env.popups();
    let hf = p.iter().rposition(|s| matches!(s, PopupState::HandsFree { .. })).unwrap();
    assert!(p.iter().rposition(|s| *s == notice).unwrap() > hf);
}

#[test]
fn an_error_during_a_hold_is_shown_when_the_hold_ends() {
    let r = rig();
    r.env.replies.lock().push_back((Err(EngineError::Internal("x".into())), 150));
    r.env.replies.lock().push_back((Err(EngineError::Internal("x".into())), 0));
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || r.env.recorder.active.lock().is_none());
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("saved untranscribed", 2_000, || r.env.rows().len() == 1);
    sleep(50);
    let error = PopupState::Error { message: NOT_TRANSCRIBED_MESSAGE.into() };
    assert!(!r.env.popups().contains(&error));
    r.hk.send(HotkeyEvent::Tap).unwrap();
    wait_until("error shown", 1_000, || r.env.popups().last() == Some(&error));
}

#[test]
fn a_tap_does_not_hide_an_earlier_notice() {
    let r = rig();
    r.env.paste_results.lock().push_back(PasteResult::FocusChanged);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    let notice = PopupState::NotPasted { text: "Hello there.".into() };
    wait_until("notice", 2_000, || r.env.popups().last() == Some(&notice));
    let before = r.env.events().len();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::Tap).unwrap(); // Ctrl+Win+Right style: inside the popup delay
    wait_until("cancel", 1_000, || r.env.recorder.cancels.load(Ordering::SeqCst) == 1);
    sleep(POPUP_DELAY_MS + 50);
    let after: Vec<Ev> = r.env.events()[before..].to_vec();
    assert!(!after.contains(&Ev::Hide) && !after.contains(&Ev::Popup(PopupState::Hidden)), "{after:?}");
}

#[test]
fn a_notice_covered_by_a_recording_comes_back_when_it_is_discarded() {
    let r = rig();
    r.env.paste_results.lock().push_back(PasteResult::FocusChanged);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    let notice = PopupState::NotPasted { text: "Hello there.".into() };
    wait_until("notice", 2_000, || r.env.popups().last() == Some(&notice));
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("listening", 1_000, || r.env.popups().last() == Some(&PopupState::Listening { cpu: false }));
    r.hk.send(HotkeyEvent::Tap).unwrap();
    wait_until("notice again", 1_000, || r.env.popups().last() == Some(&notice));
}

#[test]
fn the_warm_up_grace_is_measured_to_the_moment_of_pasting() {
    let r = rig();
    *r.env.status.lock() = EngineStatus::WarmingUp;
    r.env.reply("First one.", 0);
    r.env.reply("Second one.", 0);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("first stopped", 1_000, || r.env.recorder.active.lock().is_none());
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    sleep(300);
    // Ready well inside the grace, but the first paste must wait for the held chord past it.
    *r.env.status.lock() = EngineStatus::Ready { model: "m".into(), device: "cuda".into(), vram_mb: 1 };
    sleep(WARMUP_PASTE_GRACE_MS);
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 5_300 }).unwrap();
    wait_until("second pasted", 3_000, || r.env.pastes().len() == 1);
    assert_eq!(r.env.pastes(), vec!["Second one.".to_string()], "the late first one is saved, not pasted");
    wait_until("rows", 1_000, || r.env.rows().len() == 2);
}

#[test]
fn the_target_is_read_before_the_recording_stops() {
    let r = rig();
    r.env.recorder.switch_fg_on_stop.store(true, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("row", 2_000, || r.env.rows().len() == 1);
    assert_eq!(r.env.rows()[0].target_process, "msedge.exe");
}

#[test]
fn quitting_saves_a_long_hands_free_recording() {
    let r = rig();
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    assert!(r.session.shutdown(Duration::from_millis(QUIT_DRAIN_MAX_MS)), "drained");
    let rows = r.env.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].final_text, "Hello there.", "transcribed");
    assert!(r.env.pastes().is_empty(), "never pasted at quit");
}

#[test]
fn quitting_with_the_engine_not_ready_keeps_the_audio_untranscribed() {
    let r = rig();
    *r.env.status.lock() = EngineStatus::WarmingUp;
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    let t = Instant::now();
    assert!(r.session.shutdown(Duration::from_millis(QUIT_DRAIN_MAX_MS)));
    assert!(t.elapsed() < Duration::from_millis(QUIT_ENGINE_WAIT_MS + 1_000), "{:?}", t.elapsed());
    let rows = r.env.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].paste_outcome, PasteOutcome::Failed);
    assert_eq!(rows[0].final_text, "");
    assert_eq!(std::fs::read_dir(r.env.audio_dir()).unwrap().count(), 1);
}

#[test]
fn quitting_drops_a_short_recording() {
    let r = rig();
    r.env.recorder.elapsed.store(CANCEL_SAVE_AFTER_MS - 1, Ordering::SeqCst);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    assert!(r.session.shutdown(Duration::from_millis(QUIT_DRAIN_MAX_MS)));
    assert!(r.env.rows().is_empty());
    assert_eq!(r.env.recorder.cancels.load(Ordering::SeqCst), 1);
}

#[test]
fn ai_step_runs_only_when_switched_on() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("paste", 2_000, || r.env.pastes().len() == 1);
    assert_eq!(r.env.ai_calls.load(Ordering::SeqCst), 0);
    r.env.settings.lock().ai_cleanup.enabled = true;
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("second paste", 2_000, || r.env.pastes().len() == 2);
    assert_eq!(r.env.ai_calls.load(Ordering::SeqCst), 1);
    assert_eq!(r.env.pastes()[1], "Hello, there.");
}

#[test]
fn sounds_play_only_when_switched_on() {
    let r = rig();
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    sleep(POPUP_DELAY_MS + 50);
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("paste", 2_000, || r.env.pastes().len() == 1);
    assert!(!r.env.events().iter().any(|e| matches!(e, Ev::Sound(_))));
    r.env.settings.lock().sounds = true;
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    wait_until("start tick", 1_000, || r.env.events().contains(&Ev::Sound(Sound::Start)));
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 2_000 }).unwrap();
    wait_until("stop tick", 1_000, || r.env.events().contains(&Ev::Sound(Sound::Stop)));
}

#[test]
fn pause_and_reinstall_reach_the_hotkey_and_pause_drops_a_recording() {
    let r = rig();
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("recording", 1_000, || r.env.recorder.starts.load(Ordering::SeqCst) == 1);
    r.session.send(Msg::SetPaused(true));
    r.session.send(Msg::ReinstallHook);
    wait_until("hotkey commands", 1_000, || {
        let ev = r.env.events();
        ev.contains(&Ev::Hotkey(HotkeyCmd::SetPaused(true))) && ev.contains(&Ev::Hotkey(HotkeyCmd::Reinstall))
    });
    assert_eq!(r.env.recorder.cancels.load(Ordering::SeqCst), 1);
}

#[test]
fn a_result_does_not_cover_a_new_recording() {
    let r = rig();
    r.env.reply("Slow one.", 400);
    r.hk.send(HotkeyEvent::HoldStart).unwrap();
    r.hk.send(HotkeyEvent::HoldEnd { held_ms: 1_000 }).unwrap();
    wait_until("stopped", 1_000, || r.env.recorder.active.lock().is_none());
    // Hands-free (keys up), so the first one pastes while it records; a hold would make it wait.
    r.hk.send(HotkeyEvent::HandsFreeStart).unwrap();
    wait_until("hands-free", 1_000, || r.env.popups().iter().any(|p| matches!(p, PopupState::HandsFree { .. })));
    wait_until("first pasted", 2_000, || r.env.pastes().len() == 1);
    sleep(50);
    assert!(matches!(r.env.popups().last(), Some(PopupState::HandsFree { .. })), "{:?}", r.env.popups());
    r.session.send(Msg::Discard);
}
