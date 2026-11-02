//! The correction watcher's tracking logic (spec §8), pure and driven by injected reads, sleeps
//! and a clock so every rule is unit-tested without UI Automation:
//!
//! - first read `WATCH_FIRST_READ_MS` after the paste, one retry `WATCH_RETRY_MS` later if empty;
//! - the pasted span is located inside the box's text by the text around it (dictating into an
//!   existing draft still learns);
//! - then a poll every `WATCH_POLL_MS` while less than `WATCH_FAST_PHASE_MS` has passed since the
//!   paste or the last change, every `WATCH_POLL_SLOW_MS` after that, up to `WATCH_MAX_MS`;
//! - stop when focus moves to another element, the box empties (message sent), the text stops
//!   resembling the last snapshot, `WATCH_MAX_FAILURES` failures in a row, `WATCH_MAX_SLOW_CALLS`
//!   slow reads, the box grows too long, or the caller cancels (the next dictation started).
//!
//! The last non-empty snapshot is kept; the caller diffs the pasted span with its edited version.

use crate::constants::{
    WATCH_FAST_PHASE_MS, WATCH_FIRST_READ_MS, WATCH_MAX_FAILURES, WATCH_MAX_FIELD_CHARS, WATCH_MAX_FIELD_FACTOR,
    WATCH_MAX_MS, WATCH_MAX_SLOW_CALLS, WATCH_POLL_MS, WATCH_POLL_SLOW_MS, WATCH_RETRY_MS,
    WATCH_SIMILAR_MIN_DICE, WATCH_SIMILAR_MIN_LEN_RATIO, WATCH_SIMILAR_MIN_LEVENSHTEIN, WATCH_SIMILAR_SHORT_CHARS,
    WATCH_SLOW_CALL_MS,
};

/// Which element had focus: its process and UI Automation runtime id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusKey {
    pub pid: u32,
    pub runtime_id: Vec<i32>,
}

impl FocusKey {
    /// Same element (an unknown runtime id compares by process only).
    pub fn same_as(&self, other: &FocusKey) -> bool {
        self.pid == other.pid && (self.runtime_id.is_empty() || other.runtime_id.is_empty() || self.runtime_id == other.runtime_id)
    }
}

/// One read of the focused element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    /// The focused element and its whole text.
    Text { focus: FocusKey, text: String, took_ms: u64 },
    /// The focused element offers no text (no TextPattern or ValuePattern).
    NoText { focus: FocusKey, took_ms: u64 },
    /// UI Automation failed.
    Failed { took_ms: u64 },
}

impl Reading {
    fn took_ms(&self) -> u64 {
        match self {
            Reading::Text { took_ms, .. } | Reading::NoText { took_ms, .. } | Reading::Failed { took_ms } => *took_ms,
        }
    }
}

/// Why a watch ended. Names are for logs (no text).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Nothing readable at the first read and its retry.
    NothingRead,
    /// The pasted text is not in the box.
    NotFound,
    FocusMoved,
    /// The box emptied (message sent or cleared).
    Emptied,
    /// The text stopped resembling the last snapshot (another box, or a rewrite).
    Diverged,
    Failures,
    TooSlow,
    TooLong,
    TimeUp,
    /// The caller stopped it (the next dictation started, or Wren is quitting).
    Cancelled,
}

/// How a watch ended and what it saw (text stays in memory; never logged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEnd {
    pub reason: EndReason,
    /// The pasted text as it appeared in the box at the first read.
    pub pasted_span: Option<String>,
    /// The same span in the last non-empty snapshot that still resembled the dictation.
    pub edited_span: Option<String>,
    pub reads: u32,
    pub slow_calls: u32,
}

/// Text as compared by the watcher: line breaks as `\n`, no-break spaces as spaces, invisible
/// placeholders (object replacement, zero-width) removed.
pub fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            '\u{00A0}' | '\u{202F}' => out.push(' '),
            '\u{FFFC}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}' => {}
            c => out.push(c),
        }
    }
    out
}

/// Only whitespace or invisible characters (how an empty message box reads: Chromium gives an empty
/// ProseMirror composer as one line break).
pub fn is_blank(s: &str) -> bool {
    normalise(s).trim().is_empty()
}

fn dice<T: Eq + std::hash::Hash>(a: Vec<T>, b: Vec<T>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    // Multiset intersection.
    let mut counts = std::collections::HashMap::new();
    for x in &a {
        *counts.entry(x).or_insert(0i64) += 1;
    }
    let mut common = 0usize;
    for x in &b {
        if let Some(n) = counts.get_mut(x)
            && *n > 0
        {
            *n -= 1;
            common += 1;
        }
    }
    2.0 * common as f64 / (a.len() + b.len()) as f64
}

fn word_bigrams(s: &str) -> Vec<(String, String)> {
    let words: Vec<String> = s.split_whitespace().map(str::to_string).collect();
    words.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect()
}

fn char_grams(s: &str, n: usize) -> Vec<Vec<char>> {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() < n {
        return vec![chars];
    }
    chars.windows(n).map(<[char]>::to_vec).collect()
}

/// Willow's "still resembles" test on lower-cased, trimmed text: the shorter is at least
/// `WATCH_SIMILAR_MIN_LEN_RATIO` of the longer; short texts by Levenshtein similarity, longer ones by
/// word-bigram (or character-4-gram) Dice.
pub fn similar(a: &str, b: &str) -> bool {
    let (a, b) = (normalise(a).trim().to_lowercase(), normalise(b).trim().to_lowercase());
    if a == b {
        return true;
    }
    let (la, lb) = (a.chars().count(), b.chars().count());
    let (short, long) = (la.min(lb), la.max(lb));
    if long == 0 || (short as f64) / (long as f64) < WATCH_SIMILAR_MIN_LEN_RATIO {
        return false;
    }
    if la <= WATCH_SIMILAR_SHORT_CHARS && lb <= WATCH_SIMILAR_SHORT_CHARS {
        return strsim::normalized_levenshtein(&a, &b) >= WATCH_SIMILAR_MIN_LEVENSHTEIN;
    }
    let score = if a.split_whitespace().count() >= 2 && b.split_whitespace().count() >= 2 {
        dice(word_bigrams(&a), word_bigrams(&b))
    } else {
        dice(char_grams(&a, 4), char_grams(&b, 4))
    };
    score >= WATCH_SIMILAR_MIN_DICE
}

/// Where the pasted text sits in the box: a char range of `normalise(field)`. The last occurrence
/// wins (a dictation usually lands at the end of a draft).
pub fn locate(field: &str, pasted: &str) -> Option<std::ops::Range<usize>> {
    let needle = normalise(pasted);
    let needle = needle.trim();
    if needle.is_empty() {
        return None;
    }
    let hay = normalise(field);
    let byte = hay.rfind(needle)?;
    let start = hay[..byte].chars().count();
    Some(start..start + needle.chars().count())
}

/// The span's text in `now` (both fields normalised), found from what is unchanged around it: the
/// text before the span must still start `now` and the text after it must still end it. `None` if
/// the edit reached outside the span.
pub fn edited_span(base: &str, span: std::ops::Range<usize>, now: &str) -> Option<String> {
    let old: Vec<char> = normalise(base).chars().collect();
    let new: Vec<char> = normalise(now).chars().collect();
    if span.end > old.len() || span.start > span.end {
        return None;
    }
    let before = &old[..span.start];
    let after = &old[span.end..];
    if new.len() < before.len() + after.len() || !new.starts_with(before) || !new.ends_with(after) {
        return None;
    }
    Some(new[before.len()..new.len() - after.len()].iter().collect())
}

/// Longest box worth watching for a dictation of `pasted_chars`.
fn max_field_chars(pasted_chars: usize) -> usize {
    (pasted_chars * WATCH_MAX_FIELD_FACTOR).max(WATCH_MAX_FIELD_CHARS)
}

/// Runs one watch. `read` reads the focused element; `sleep(ms)` waits and returns false when the
/// watch is cancelled; `now()` is milliseconds since the paste.
pub fn watch(
    pasted: &str,
    read: &mut dyn FnMut() -> Reading,
    sleep: &mut dyn FnMut(u64) -> bool,
    now: &dyn Fn() -> u64,
) -> WatchEnd {
    let mut end = WatchEnd { reason: EndReason::Cancelled, pasted_span: None, edited_span: None, reads: 0, slow_calls: 0 };
    let max_chars = max_field_chars(pasted.chars().count());
    let mut failures = 0u32;
    // One read, counted; the reading, or the reason to stop.
    let mut take = |end: &mut WatchEnd, failures: &mut u32| -> Result<Reading, EndReason> {
        let r = read();
        end.reads += 1;
        if r.took_ms() > WATCH_SLOW_CALL_MS {
            end.slow_calls += 1;
            if end.slow_calls >= WATCH_MAX_SLOW_CALLS {
                return Err(EndReason::TooSlow);
            }
        }
        if matches!(r, Reading::Failed { .. }) {
            *failures += 1;
            if *failures >= WATCH_MAX_FAILURES {
                return Err(EndReason::Failures);
            }
        } else {
            *failures = 0;
        }
        Ok(r)
    };

    // 1. First read, one retry if there is nothing to read yet.
    if !sleep(WATCH_FIRST_READ_MS) {
        return end;
    }
    let mut first = None;
    for attempt in 0..2 {
        if attempt == 1 && !sleep(WATCH_RETRY_MS) {
            return end;
        }
        match take(&mut end, &mut failures) {
            Err(reason) => {
                end.reason = reason;
                return end;
            }
            Ok(Reading::Text { focus, text, .. }) if !is_blank(&text) => {
                first = Some((focus, text));
                break;
            }
            Ok(_) => {}
        }
    }
    let Some((focus, base)) = first else {
        end.reason = EndReason::NothingRead;
        return end;
    };
    if base.chars().count() > max_chars {
        end.reason = EndReason::TooLong;
        return end;
    }
    let Some(span) = locate(&base, pasted) else {
        end.reason = EndReason::NotFound;
        return end;
    };
    let base = normalise(&base);
    let span_text: String = base.chars().skip(span.start).take(span.len()).collect();
    end.pasted_span = Some(span_text.clone());
    end.edited_span = Some(span_text);
    let mut last = base.clone();
    let mut last_change = now();

    // 2. Poll until something ends it.
    let reason = loop {
        let t = now();
        if t >= WATCH_MAX_MS {
            break EndReason::TimeUp;
        }
        let delay = if t.saturating_sub(last_change) < WATCH_FAST_PHASE_MS { WATCH_POLL_MS } else { WATCH_POLL_SLOW_MS };
        if !sleep(delay.min(WATCH_MAX_MS - t)) {
            break EndReason::Cancelled;
        }
        let r = match take(&mut end, &mut failures) {
            Ok(r) => r,
            Err(reason) => break reason,
        };
        match r {
            Reading::Failed { .. } => continue,
            Reading::NoText { focus: f, .. } => {
                if !f.same_as(&focus) {
                    break EndReason::FocusMoved;
                }
                // Same element but no text this time: a hiccup, counted like a failure.
                failures += 1;
                if failures >= WATCH_MAX_FAILURES {
                    break EndReason::Failures;
                }
            }
            Reading::Text { focus: f, text, .. } => {
                if !f.same_as(&focus) {
                    break EndReason::FocusMoved;
                }
                if is_blank(&text) {
                    break EndReason::Emptied;
                }
                if text.chars().count() > max_chars {
                    break EndReason::TooLong;
                }
                let text = normalise(&text);
                if !similar(&last, &text) {
                    break EndReason::Diverged;
                }
                if text != last {
                    last = text;
                    last_change = now();
                    end.edited_span = edited_span(&base, span.clone(), &last);
                }
            }
        }
    };
    end.reason = reason;
    end
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn normalise_and_blank() {
        assert_eq!(normalise("a\r\nb\rc\u{00A0}d\u{FFFC}\u{200B}e"), "a\nb\nc de");
        for blank in ["", "\n", " \r\n ", "\u{FFFC}", "\u{200B}\n", "\u{00A0}"] {
            assert!(is_blank(blank), "{blank:?}");
        }
        assert!(!is_blank(" a "));
    }

    #[test]
    fn similarity_follows_willow() {
        assert!(similar("hello klode", "hello Claude"));
        assert!(similar("", ""));
        assert!(!similar("hello klode", "completely different words here"));
        assert!(!similar("a reasonably long sentence about nothing", "hi"), "length ratio");
        let long = "I think we should ship the new build to the testers on Friday after lunch";
        assert!(similar(long, &long.replace("Friday", "Thursday")));
        assert!(!similar(long, "Meanwhile the weather in London has been grey and drizzly all week long"));
        assert!(similar("ONE TWO", "one two"), "case-insensitive");
    }

    #[test]
    fn locate_finds_the_dictation_inside_a_draft() {
        assert_eq!(locate("hello klode", "hello klode"), Some(0..11));
        // An existing draft before and after; CRLF normalised; char (not byte) indices.
        let field = "Dear Sam,\r\nhello klode é\r\nBest";
        let span = locate(field, "hello klode é").unwrap();
        let n: Vec<char> = normalise(field).chars().collect();
        assert_eq!(n[span.clone()].iter().collect::<String>(), "hello klode é");
        assert_eq!(span, 10..23);
        // Last occurrence wins; trailing whitespace of the paste is ignored.
        assert_eq!(locate("ok. ok.", "ok. "), Some(4..7));
        assert_eq!(locate("nothing here", "klode"), None);
        assert_eq!(locate("anything", "  "), None);
    }

    #[test]
    fn edited_span_needs_the_edit_inside_the_span() {
        let base = "Dear Sam,\nhello klode\nBest";
        let span = locate(base, "hello klode").unwrap();
        assert_eq!(edited_span(base, span.clone(), "Dear Sam,\nhello Claude\nBest").as_deref(), Some("hello Claude"));
        assert_eq!(edited_span(base, span.clone(), base).as_deref(), Some("hello klode"), "unchanged");
        assert_eq!(edited_span(base, span.clone(), "Dear Tom,\nhello Claude\nBest"), None, "edited outside too");
        // Typing after the dictation stays with the span (the diff ignores pure insertions).
        assert_eq!(edited_span(base, span.clone(), "Dear Sam,\nhello klode!!\nBest").as_deref(), Some("hello klode!!"));
        // CRLF in the new snapshot is fine.
        assert_eq!(edited_span(base, span, "Dear Sam,\r\nhello Claude\r\nBest").as_deref(), Some("hello Claude"));
    }

    fn key(id: i32) -> FocusKey {
        FocusKey { pid: 7, runtime_id: vec![42, id] }
    }

    /// A fake box: `script(t)` says what a read at time t returns. Each read takes `took` ms.
    struct Rig {
        t: Cell<u64>,
        read_times: RefCell<Vec<u64>>,
    }

    fn run(pasted: &str, cancel_at: Option<u64>, script: impl Fn(u64) -> Reading) -> (WatchEnd, Vec<u64>) {
        let rig = Rig { t: Cell::new(0), read_times: RefCell::new(Vec::new()) };
        let mut read = || {
            let now = rig.t.get();
            rig.read_times.borrow_mut().push(now);
            script(now)
        };
        let mut sleep = |ms: u64| {
            let next = rig.t.get() + ms;
            if cancel_at.is_some_and(|c| next >= c) {
                return false;
            }
            rig.t.set(next);
            true
        };
        let now = || rig.t.get();
        let end = watch(pasted, &mut read, &mut sleep, &now);
        let times = rig.read_times.borrow().clone();
        (end, times)
    }

    fn text(t: &str) -> Reading {
        Reading::Text { focus: key(1), text: t.into(), took_ms: 1 }
    }

    #[test]
    fn fix_then_send_learns_from_the_last_snapshot() {
        let (end, times) = run("hello klode", None, |t| match t {
            ..2_000 => text("hello klode"),
            ..4_000 => text("hello Claude"),
            _ => text("\n"), // sent: an empty composer reads as a line break
        });
        assert_eq!(end.reason, EndReason::Emptied);
        assert_eq!(end.pasted_span.as_deref(), Some("hello klode"));
        assert_eq!(end.edited_span.as_deref(), Some("hello Claude"));
        assert_eq!(times[0], WATCH_FIRST_READ_MS);
        assert_eq!(times[1] - times[0], WATCH_POLL_MS);
        assert_eq!(end.reads as usize, times.len());
    }

    #[test]
    fn empty_first_read_is_retried_once() {
        let (end, times) = run("hello klode", Some(3_000), |t| if t < 500 { text("") } else { text("hello klode") });
        assert_eq!(times[0], WATCH_FIRST_READ_MS);
        assert_eq!(times[1], WATCH_FIRST_READ_MS + WATCH_RETRY_MS);
        assert_eq!(end.reason, EndReason::Cancelled);
        assert_eq!(end.edited_span.as_deref(), Some("hello klode"));

        let (end, times) = run("hello klode", None, |_| text("\n"));
        assert_eq!(end.reason, EndReason::NothingRead);
        assert_eq!(times.len(), 2);
        assert_eq!(end.pasted_span, None);
    }

    #[test]
    fn a_dictation_that_is_not_in_the_box_is_not_watched() {
        let (end, times) = run("hello klode", None, |_| text("something else entirely"));
        assert_eq!(end.reason, EndReason::NotFound);
        assert_eq!(times.len(), 1);
    }

    #[test]
    fn focus_moving_ends_the_watch_and_keeps_the_last_snapshot() {
        let (end, _) = run("hello klode", None, |t| match t {
            ..1_000 => text("hello klode"),
            ..2_000 => text("hello Claude"),
            _ => Reading::Text { focus: key(2), text: "another box".into(), took_ms: 1 },
        });
        assert_eq!(end.reason, EndReason::FocusMoved);
        assert_eq!(end.edited_span.as_deref(), Some("hello Claude"));
        // Focus in another app (different process) too.
        let (end, _) = run("hello klode", None, |t| match t {
            ..1_000 => text("hello klode"),
            _ => Reading::NoText { focus: FocusKey { pid: 9, runtime_id: vec![1] }, took_ms: 1 },
        });
        assert_eq!(end.reason, EndReason::FocusMoved);
    }

    #[test]
    fn diverging_text_ends_the_watch() {
        let (end, _) = run("hello klode", None, |t| match t {
            ..1_000 => text("hello klode"),
            _ => text("a totally unrelated paragraph of text that replaced it"),
        });
        assert_eq!(end.reason, EndReason::Diverged);
        assert_eq!(end.edited_span.as_deref(), Some("hello klode"));
    }

    #[test]
    fn three_failures_in_a_row_end_it_but_a_good_read_resets_the_count() {
        let (end, times) = run("hello klode", None, |t| match t {
            ..300 => text("hello klode"),
            750 | 1_250 => Reading::Failed { took_ms: 1 },
            ..2_000 => text("hello Claude"),
            _ => Reading::Failed { took_ms: 1 },
        });
        assert_eq!(end.reason, EndReason::Failures);
        assert_eq!(end.edited_span.as_deref(), Some("hello Claude"));
        let failures_at_end = times.iter().filter(|t| **t >= 2_000).count();
        assert_eq!(failures_at_end as u32, WATCH_MAX_FAILURES);
    }

    #[test]
    fn three_slow_reads_end_it_as_too_slow() {
        let (end, _) = run("hello klode", None, |t| {
            let took_ms = if t >= 1_000 { WATCH_SLOW_CALL_MS + 1 } else { 1 };
            Reading::Text { focus: key(1), text: "hello klode".into(), took_ms }
        });
        assert_eq!(end.reason, EndReason::TooSlow);
        assert_eq!(end.slow_calls, WATCH_MAX_SLOW_CALLS);
    }

    #[test]
    fn polls_slow_down_after_quiet_and_stop_at_the_cap() {
        let (end, times) = run("hello klode", None, |_| text("hello klode"));
        assert_eq!(end.reason, EndReason::TimeUp);
        let gaps: Vec<u64> = times.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().take_while(|g| **g == WATCH_POLL_MS).count() >= 18, "fast phase: {gaps:?}");
        // Slow polls after the quiet spell (the very last one is cut short by the cap).
        assert!(gaps.iter().rev().skip(1).take(10).all(|g| *g == WATCH_POLL_SLOW_MS), "{gaps:?}");
        assert!(*times.last().unwrap() <= WATCH_MAX_MS);
        // A change puts it back on the fast poll.
        let (_, times) = run("hello klode", Some(40_000), |t| if t < 20_000 { text("hello klode") } else { text("hello Claude") });
        let after: Vec<u64> = times.iter().copied().filter(|t| *t > 20_000).collect();
        assert!(after.windows(2).any(|w| w[1] - w[0] == WATCH_POLL_MS), "{after:?}");
    }

    #[test]
    fn a_huge_box_is_not_watched() {
        let big = "x ".repeat(WATCH_MAX_FIELD_CHARS) + "hello klode";
        let (end, _) = run("hello klode", None, move |_| text(&big));
        assert_eq!(end.reason, EndReason::TooLong);
    }

    #[test]
    fn cancel_ends_it_with_what_was_seen() {
        let (end, _) = run("hello klode", Some(1_600), |t| if t < 1_000 { text("hello klode") } else { text("hello Claude") });
        assert_eq!(end.reason, EndReason::Cancelled);
        assert_eq!(end.edited_span.as_deref(), Some("hello Claude"));
        let (end, times) = run("hello klode", Some(100), |_| text("hello klode"));
        assert_eq!((end.reason, times.len()), (EndReason::Cancelled, 0));
    }
}
