//! Correction watcher (spec §8): after a successful paste, read the text box for a while with UI
//! Automation; when the user fixes a word Wren mis-heard, learn it so it comes out right next time.
//!
//! - [`track`]: the pure tracking rules (when to read, when to stop, where the dictation sits).
//! - [`uia`]: UI Automation reads (read-only).
//! - [`prepaste`]: the pre-paste editable check (ruling 24).
//! - this file: whether to watch at all, the watch thread, and turning a finished watch into
//!   dictionary entries plus the "Learned: X → Y · Undo" toast.
//!
//! One watch at a time: a new dictation (or a new paste) stops the last one. Each watch runs on its
//! own thread in the COM multithreaded apartment, so a hung UI Automation call never holds up the
//! session, and the stop flag is checked between reads.
//!
//! Privacy: text stays in memory and goes only to the diff and the store. Logs carry the end
//! reason, counts, timings and the app's process name.

pub mod prepaste;
pub mod track;
#[cfg(windows)]
pub mod uia;
#[cfg(target_os = "macos")]
pub mod ax_macos;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;

use crate::constants::{
    WATCH_MAX_SLOW_CALLS, WATCH_MIN_PASTE_CHARS, WATCH_SLOW_CALL_MS, WATCH_STOP_CHECK_MS, WATCH_UIA_TIMEOUT_MS,
};
use crate::dictionary::Source;
use crate::dictionary::learn::{LearnDecision, diff_corrections};
use crate::session::{PastedDictation, PopupState};
use crate::store::Store;
use track::{EndReason, FocusKey, Reading, WatchEnd};

/// Never watched: what is typed there is commands, not prose.
pub const TERMINALS: [&str; 14] = [
    "windowsterminal.exe",
    "conhost.exe",
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "terminal",
    "iterm2",
    "iterm",
    "ghostty",
    "warp",
    "alacritty",
    "wezterm",
    "wezterm-gui",
    "kitty",
];

/// Result of the read-only probe of the Claude desktop app (docs/research/claude-uia-probe.md,
/// 2 November 2026): the message box read in p95 0 ms (max 1 ms) over 20 reads, well under the
/// 50 ms bar, so the watcher is on unless the user switches it off.
pub const PROBE_VERDICT: &str = "readable";
pub const PROBE_REASON: &str = "The Claude app's text box can be read in under 1 ms, so Wren watches it after pasting.";
pub const PROBE_DEFAULT_ON: bool = true;

/// `watcher_status` (contract with src/lib/types.ts `WatcherStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatcherStatus {
    /// The effective state (the setting, or the probe's default when the setting is automatic).
    pub enabled: bool,
    pub verdict: Option<String>,
    pub reason: Option<String>,
}

/// The setting (`None` = automatic) resolved against the probe's verdict.
pub fn effective_enabled(setting: Option<bool>) -> bool {
    setting.unwrap_or(PROBE_DEFAULT_ON)
}

/// What Settings shows. `slow_apps` are the apps switched off this run for being slow to read.
pub fn status(setting: Option<bool>, slow_apps: &[String]) -> WatcherStatus {
    let mut reason = PROBE_REASON.to_string();
    if !slow_apps.is_empty() {
        reason.push_str(&format!(
            " Paused until Wren restarts for {}: its text box was too slow to read.",
            slow_apps.join(", ")
        ));
    }
    WatcherStatus { enabled: effective_enabled(setting), verdict: Some(PROBE_VERDICT.to_string()), reason: Some(reason) }
}

/// Why a paste is not watched (`None` = watch it). For logs; no text.
pub fn skip_reason(process_name: &str, text: &str, enabled: bool, slow_apps: &HashSet<String>) -> Option<&'static str> {
    let app = process_name.to_lowercase();
    if !enabled {
        Some("switched off")
    } else if TERMINALS.contains(&app.as_str()) {
        Some("terminal")
    } else if slow_apps.contains(&app) {
        Some("too slow to read in this app")
    } else if text.trim().chars().count() < WATCH_MIN_PASTE_CHARS {
        Some("too short")
    } else {
        None
    }
}

/// One correction the watch turned into a dictionary change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Learned {
    pub from: String,
    pub to: String,
    pub decision: LearnDecision,
    /// The entry the toast's Undo switches off: the replacement, or the term when only that was new.
    pub entry_id: Option<i64>,
}

/// The pasted span and its edited version from a finished watch → `store.learn(LearnedPaste, …)`
/// for each correction the diff accepts.
pub fn learn_from(store: &Store, transcription_id: &str, end: &WatchEnd) -> Vec<Learned> {
    let (Some(pasted), Some(edited)) = (&end.pasted_span, &end.edited_span) else { return Vec::new() };
    if pasted == edited || !track::similar(pasted, edited) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for c in diff_corrections(pasted, edited) {
        let (from, to) = (c.from.clone(), c.to.clone());
        let decision = match store.learn(Source::LearnedPaste, transcription_id, c) {
            Ok(d) => d,
            Err(err) => {
                tracing::warn!(error = %err, "cannot save a learned correction");
                continue;
            }
        };
        let entry_id = match &decision {
            LearnDecision::AddedReplacement => store.replacement_id(&from, &to).ok().flatten(),
            LearnDecision::AddedTerm => store.term_id(&to).ok().flatten(),
            _ => None,
        };
        out.push(Learned { from, to, decision, entry_id });
    }
    out
}

/// The popup toast for what was learned: the first new replacement or term.
pub fn toast_for(learned: &[Learned]) -> Option<PopupState> {
    learned
        .iter()
        .find(|l| matches!(l.decision, LearnDecision::AddedReplacement | LearnDecision::AddedTerm) && l.entry_id.is_some())
        .map(|l| PopupState::Learned { from: l.from.clone(), to: l.to.clone(), entry_id: l.entry_id.unwrap_or_default() })
}

/// Runs watches, one at a time. Cheap to clone.
#[derive(Clone, Default)]
pub struct Watcher {
    generation: Arc<AtomicU64>,
    slow_apps: Arc<Mutex<HashSet<String>>>,
}

impl Watcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stops the running watch (if any) at its next read.
    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn slow_apps(&self) -> Vec<String> {
        let mut v: Vec<String> = self.slow_apps.lock().iter().cloned().collect();
        v.sort();
        v
    }

    /// Watches `d` if it should be (see [`skip_reason`]); `on_end` runs on the watch thread when it
    /// ends, with nothing of Wren's locked. Returns whether a watch started.
    pub fn start(&self, d: PastedDictation, enabled: bool, on_end: impl FnOnce(&PastedDictation, WatchEnd) + Send + 'static) -> bool {
        let skip = skip_reason(&d.target.process_name, &d.text, enabled, &self.slow_apps.lock());
        if let Some(why) = skip {
            tracing::debug!(id = %d.id, app = %d.target.process_name, why, "not watching");
            return false;
        }
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let generation = self.generation.clone();
        let slow_apps = self.slow_apps.clone();
        let spawned = std::thread::Builder::new().name("wren-watcher".into()).spawn(move || {
            let started = Instant::now();
            #[cfg(windows)]
            let _com = uia::ComMta::join();
            #[cfg(windows)]
            let client = match uia::Uia::new(WATCH_UIA_TIMEOUT_MS) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(error = %err, "watcher: no UI Automation");
                    return;
                }
            };
            #[cfg(target_os = "macos")]
            let client = match ax_macos::AxClient::new(WATCH_UIA_TIMEOUT_MS) {
                Ok(c) => c,
                Err(err) => {
                    tracing::warn!(error = %err, "watcher: no macOS Accessibility client");
                    return;
                }
            };
            let target_pid = d.target.pid;
            let max_chars = track::max_field_chars(d.text.chars().count());
            let mut read = |expected: Option<&FocusKey>| read_focused(&client, target_pid, expected, max_chars);
            let live = || generation.load(Ordering::SeqCst) == my_gen;
            let mut sleep = |ms: u64| sleep_while(ms, &live);
            let now = || started.elapsed().as_millis() as u64;
            let end = track::watch(&d.text, &mut read, &mut sleep, &now);
            let app = d.target.process_name.clone();
            if end.reason == EndReason::TooSlow {
                tracing::warn!(
                    %app,
                    slow_calls = end.slow_calls,
                    "watcher switched off for this app until Wren restarts: {WATCH_MAX_SLOW_CALLS} reads took over {WATCH_SLOW_CALL_MS} ms"
                );
                slow_apps.lock().insert(app.clone());
            }
            tracing::info!(
                id = %d.id,
                %app,
                reason = ?end.reason,
                reads = end.reads,
                slow_calls = end.slow_calls,
                watched_ms = now(),
                changed = end.pasted_span != end.edited_span,
                "watch ended"
            );
            on_end(&d, end);
        });
        if let Err(err) = spawned {
            tracing::warn!(error = %err, "cannot start the watcher thread");
            return false;
        }
        true
    }
}

/// Sleeps `ms` in short steps; false as soon as `live()` turns false.
fn sleep_while(ms: u64, live: &dyn Fn() -> bool) -> bool {
    const STEP_MS: u64 = WATCH_STOP_CHECK_MS;
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        if !live() {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(Duration::from_millis(STEP_MS)));
    }
}

/// Whether the watcher may read the text of the focused element: it must be in the paste target's
/// process and, once a watch has found its box, be that box (review r1: never read another
/// element's text, not even to compare).
pub fn may_read(focus: &FocusKey, target_pid: u32, expected: Option<&FocusKey>) -> bool {
    focus.pid == target_pid && expected.is_none_or(|e| focus.same_as(e))
}

/// One watcher read of the focused element (text bounded to `max_chars + 1`).
#[cfg(windows)]
fn read_focused(client: &uia::Uia, target_pid: u32, expected: Option<&FocusKey>, max_chars: usize) -> Reading {
    let t = Instant::now();
    let took = || t.elapsed().as_millis() as u64;
    let focused = match client.focused() {
        Ok(f) => f,
        Err(_) => return Reading::Failed { took_ms: took() },
    };
    let focus = FocusKey { pid: focused.facts.pid, runtime_id: focused.runtime_id.clone() };
    if !may_read(&focus, target_pid, expected) {
        return Reading::NoText { focus, took_ms: took() };
    }
    match client.read_text(&focused, Some(max_chars)) {
        Ok(Some(text)) => Reading::Text { focus, text, took_ms: took() },
        Ok(None) => Reading::NoText { focus, took_ms: took() },
        Err(_) => Reading::Failed { took_ms: took() },
    }
}

/// One watcher read of the focused macOS Accessibility element (text bounded to `max_chars + 1`).
#[cfg(target_os = "macos")]
fn read_focused(client: &ax_macos::AxClient, target_pid: u32, expected: Option<&FocusKey>, max_chars: usize) -> Reading {
    let t = Instant::now();
    let took = || t.elapsed().as_millis() as u64;
    if !crate::hotkey::accessibility_trusted(false) {
        return Reading::Failed { took_ms: took() };
    }
    let focused = match client.focused() {
        Ok(f) => f,
        Err(_) => return Reading::Failed { took_ms: took() },
    };
    let focus = FocusKey { pid: focused.facts.pid, runtime_id: focused.runtime_id.clone() };
    if !may_read(&focus, target_pid, expected) {
        return Reading::NoText { focus, took_ms: took() };
    }
    match client.read_text(&focused, Some(max_chars)) {
        Ok(Some(text)) => Reading::Text { focus, text, took_ms: took() },
        Ok(None) => Reading::NoText { focus, took_ms: took() },
        Err(_) => Reading::Failed { took_ms: took() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dictionary::{EntryKind, Status};

    fn store() -> Store {
        let dir = std::env::temp_dir().join(format!("wren-watcher-test-{}", uuid::Uuid::new_v4()));
        Store::open(&dir.join("wren.db")).unwrap()
    }

    fn end(pasted: &str, edited: &str) -> WatchEnd {
        WatchEnd {
            reason: EndReason::Emptied,
            pasted_span: Some(pasted.into()),
            edited_span: Some(edited.into()),
            reads: 5,
            slow_calls: 0,
        }
    }

    #[test]
    fn terminals_short_text_slow_apps_and_the_switch_skip_the_watch() {
        let none = HashSet::new();
        assert_eq!(skip_reason("claude.exe", "hello klode", true, &none), None);
        for t in TERMINALS {
            assert_eq!(skip_reason(t, "hello klode", true, &none), Some("terminal"), "{t}");
        }
        assert_eq!(skip_reason("WindowsTerminal.exe", "hello klode", true, &none), Some("terminal"));
        assert_eq!(skip_reason("claude.exe", "hello klode", false, &none), Some("switched off"));
        assert_eq!(skip_reason("claude.exe", " ok ", true, &none), Some("too short"));
        let slow = HashSet::from(["slowapp.exe".to_string()]);
        assert_eq!(skip_reason("SlowApp.exe", "hello klode", true, &slow), Some("too slow to read in this app"));
    }

    #[test]
    fn status_follows_the_setting_and_the_probe() {
        assert_eq!(effective_enabled(None), PROBE_DEFAULT_ON);
        assert!(effective_enabled(Some(true)));
        assert!(!effective_enabled(Some(false)));
        let s = status(None, &[]);
        assert_eq!(s.verdict.as_deref(), Some("readable"));
        assert!(s.enabled);
        assert!(!status(Some(false), &[]).enabled);
        assert!(status(None, &["slowapp.exe".into()]).reason.unwrap().contains("slowapp.exe"));
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json.as_object().unwrap().keys().cloned().collect::<Vec<_>>(), vec!["enabled", "reason", "verdict"]);
    }

    #[test]
    fn a_fixed_unusual_word_becomes_an_active_replacement_and_a_toast() {
        let store = store();
        let learned = learn_from(&store, "t1", &end("I asked klode about it", "I asked Claude about it"));
        assert_eq!(learned.len(), 1);
        assert_eq!((learned[0].from.as_str(), learned[0].to.as_str()), ("klode", "Claude"));
        assert_eq!(learned[0].decision, LearnDecision::AddedReplacement);
        let entries = store.dictionary_list().unwrap();
        let rep = entries.iter().find(|e| e.kind == EntryKind::Replacement).unwrap();
        assert_eq!((rep.from_text.as_deref(), rep.to_text.as_str(), rep.status, rep.source), (Some("klode"), "Claude", Status::Active, Source::LearnedPaste));
        assert!(entries.iter().any(|e| e.kind == EntryKind::Term && e.to_text == "Claude" && e.status == Status::Active));
        assert_eq!(
            toast_for(&learned),
            Some(PopupState::Learned { from: "klode".into(), to: "Claude".into(), entry_id: rep.id })
        );
    }

    #[test]
    fn a_common_word_waits_for_a_second_sighting_without_a_toast() {
        let store = store();
        // "grey" → "gray": spelt alike, but "grey" is a common word: candidate first.
        let first = learn_from(&store, "t1", &end("the sky is grey today", "the sky is gray today"));
        assert_eq!(first[0].decision, LearnDecision::CandidateObserved);
        assert_eq!(toast_for(&first), None);
        let second = learn_from(&store, "t2", &end("the sky is grey today", "the sky is gray today"));
        assert_eq!(second[0].decision, LearnDecision::AddedReplacement);
        assert!(toast_for(&second).is_some());
    }

    #[test]
    fn nothing_is_learned_from_unchanged_rewritten_or_missing_spans() {
        let store = store();
        assert!(learn_from(&store, "t", &end("hello klode", "hello klode")).is_empty());
        assert!(learn_from(&store, "t", &end("hello klode", "something else entirely now")).is_empty());
        let mut e = end("hello klode", "hello Claude");
        e.edited_span = None;
        assert!(learn_from(&store, "t", &e).is_empty());
        assert!(store.dictionary_list().unwrap().is_empty());
    }

    #[test]
    fn only_the_watched_box_in_the_target_process_is_read() {
        let k = |pid: u32, id: i32| FocusKey { pid, runtime_id: vec![42, id] };
        assert!(may_read(&k(7, 1), 7, None), "first read: any element of the target");
        assert!(!may_read(&k(8, 1), 7, None), "another process");
        assert!(may_read(&k(7, 1), 7, Some(&k(7, 1))));
        assert!(!may_read(&k(7, 2), 7, Some(&k(7, 1))), "another element of the same app");
        assert!(!may_read(&FocusKey { pid: 7, runtime_id: vec![] }, 7, Some(&k(7, 1))), "unknown id");
    }

    #[test]
    fn sleep_while_stops_early_when_cancelled() {
        let t = Instant::now();
        assert!(sleep_while(30, &|| true));
        assert!(t.elapsed() >= Duration::from_millis(30));
        let t = Instant::now();
        assert!(!sleep_while(5_000, &|| false));
        assert!(t.elapsed() < Duration::from_millis(100));
    }

    fn statuses(store: &Store, word: &str) -> Vec<(EntryKind, Status)> {
        let mut v: Vec<_> = store
            .dictionary_list()
            .unwrap()
            .into_iter()
            .filter(|e| e.to_text == word)
            .map(|e| (e.kind, e.status))
            .collect();
        v.sort_by_key(|(k, _)| *k == EntryKind::Term);
        v
    }

    #[test]
    fn undo_switches_off_what_the_learn_added_but_not_older_entries() {
        // A new replacement and a new term: Undo switches both off.
        let s = store();
        let l = learn_from(&s, "t1", &end("I asked klode about it", "I asked Claude about it"));
        s.undo_learned(l[0].entry_id.unwrap()).unwrap();
        assert_eq!(statuses(&s, "Claude"), vec![(EntryKind::Replacement, Status::Disabled), (EntryKind::Term, Status::Disabled)]);

        // The term was already in the dictionary: Undo leaves it on.
        let s = store();
        s.add_term("Claude", Source::Manual).unwrap();
        let l = learn_from(&s, "t1", &end("I asked klode about it", "I asked Claude about it"));
        s.undo_learned(l[0].entry_id.unwrap()).unwrap();
        assert_eq!(statuses(&s, "Claude"), vec![(EntryKind::Replacement, Status::Disabled), (EntryKind::Term, Status::Active)]);

        // Promoted on the second sighting: the term from the first sighting goes too.
        let s = store();
        learn_from(&s, "t1", &end("the sky is grey today", "the sky is gray today"));
        let l = learn_from(&s, "t2", &end("the sky is grey today", "the sky is gray today"));
        assert_eq!(l[0].decision, LearnDecision::AddedReplacement);
        s.undo_learned(l[0].entry_id.unwrap()).unwrap();
        assert_eq!(statuses(&s, "gray"), vec![(EntryKind::Replacement, Status::Disabled), (EntryKind::Term, Status::Disabled)]);

        // A term-only toast (the replacement was already active): Undo switches off that term.
        let s = store();
        s.add_replacement("klode", "Claude", Source::Manual, Status::Active).unwrap();
        let l = learn_from(&s, "t1", &end("I asked klode about it", "I asked Claude about it"));
        assert_eq!(l[0].decision, LearnDecision::AddedTerm);
        s.undo_learned(l[0].entry_id.unwrap()).unwrap();
        assert_eq!(statuses(&s, "Claude"), vec![(EntryKind::Replacement, Status::Active), (EntryKind::Term, Status::Disabled)]);
    }
}
