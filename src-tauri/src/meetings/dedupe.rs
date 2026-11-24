//! Bleed-through removal and the dictation gate (meetings spec §5.1–5.2, plan Task 2). Pure.
//!
//! *Bleed*: on speakers without headphones, the mic hears the other people too, so a "Me" segment can
//! repeat a "Them" segment. A Me segment that overlaps a Them segment in time with ≥ 80 % word
//! similarity is dropped.
//!
//! *Echo*: the other way round, Teams can hand the user's own voice back on its output. On the Mac a
//! real call's Teams track held the mic signal ~0.4 s late at half level, so every Me line came back
//! as a second "Them" person. A Them segment that repeats a Me segment which started at least
//! [`ECHO_LEAD_MS`] earlier is that echo and is dropped. Bleed is the mirror case: the Them line
//! starts first (the mic hears the speakers at once), so a Them line that starts clearly *after* the
//! matching Me line never counts as bleed.
//!
//! *Dictation gate*: while the user dictates (hotkey held / hands-free), the meeting's Me stream must
//! not turn the dictated words into a meeting line, so Me segments overlapping a dictation window
//! (widened by 300 ms each side) are dropped.

pub const BLEED_SIMILARITY: f32 = 0.80;
/// A Them line starting this much after a matching Me line is an echo of it, not bleed.
pub const ECHO_LEAD_MS: u64 = 200;
/// How far after a Me line's end an echo of it may still start.
pub const ECHO_MAX_LAG_MS: u64 = 2_000;
pub const DICTATION_MARGIN_MS: u64 = 300;

fn words(s: &str) -> Vec<String> {
    s.split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric() || *c == '\'').collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Dice similarity over word multisets, 0..1, ignoring case and punctuation.
pub fn text_similarity(a: &str, b: &str) -> f32 {
    let (wa, mut wb) = (words(a), words(b));
    let total = wa.len() + wb.len();
    if total == 0 {
        return 0.0;
    }
    let mut common = 0usize;
    for w in &wa {
        if let Some(i) = wb.iter().position(|x| x == w) {
            wb.swap_remove(i);
            common += 1;
        }
    }
    2.0 * common as f32 / total as f32
}

/// Dice similarity over the *distinct* words: an echo transcribed with a repeat ("hello" →
/// "Hello, hello.") still matches.
pub fn distinct_word_similarity(a: &str, b: &str) -> f32 {
    let dedup = |s: &str| {
        let mut w = words(s);
        w.sort_unstable();
        w.dedup();
        w.join(" ")
    };
    text_similarity(&dedup(a), &dedup(b))
}

/// Whether a Them line starting at `them_start` started clearly after a Me line starting at
/// `me_start` (then it can be an echo of the Me line, never bleed into it).
pub fn starts_after(them_start: u64, me_start: u64) -> bool {
    them_start >= me_start + ECHO_LEAD_MS
}

/// `(start_ms, end_ms, text)` of a Me segment against the Them segments around it.
pub fn is_bleed(me: &(u64, u64, &str), them: &[(u64, u64, &str)]) -> bool {
    them.iter().any(|t| {
        me.0 < t.1 && t.0 < me.1 && !starts_after(t.0, me.0) && text_similarity(me.2, t.2) >= BLEED_SIMILARITY
    })
}

/// `(start_ms, end_ms, text)` of a Them segment against the Me segments before it: Teams echoing the
/// user's own words back.
pub fn is_echo(them: &(u64, u64, &str), me: &[(u64, u64, &str)]) -> bool {
    me.iter().any(|m| {
        starts_after(them.0, m.0)
            && them.0 < m.1 + ECHO_MAX_LAG_MS
            && distinct_word_similarity(them.2, m.2) >= BLEED_SIMILARITY
    })
}

/// Dictation windows in meeting-stream milliseconds.
#[derive(Debug, Default, Clone)]
pub struct DictationGate {
    windows: Vec<(u64, Option<u64>)>,
}

impl DictationGate {
    pub fn begin(&mut self, at_ms: u64) {
        if !matches!(self.windows.last(), Some((_, None))) {
            self.windows.push((at_ms, None));
        }
    }

    pub fn end(&mut self, at_ms: u64) {
        if let Some((_, end @ None)) = self.windows.last_mut() {
            *end = Some(at_ms);
        }
        // Keep the list short: windows that ended more than ten minutes before this one can't matter.
        self.windows.retain(|(_, e)| e.is_none_or(|e| e + 600_000 >= at_ms));
    }

    pub fn blocks(&self, start_ms: u64, end_ms: u64) -> bool {
        self.windows.iter().any(|&(b, e)| {
            let e = e.unwrap_or(u64::MAX - DICTATION_MARGIN_MS);
            start_ms < e + DICTATION_MARGIN_MS && b.saturating_sub(DICTATION_MARGIN_MS) < end_ms
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_of_me_is_dropped_from_them() {
        // Real Mac call: Me 696–5024 ms, Teams' copy 2296–5408 ms.
        let me = [(696, 5_024, "Can you hear me okay on the call today")];
        assert!(is_echo(&(2_296, 5_408, "can you hear me okay on the call today?"), &me));
        // A repeat in the transcription of the echo still matches ("hello" → "Hello, hello.").
        assert!(is_echo(&(1_400, 2_000, "Hello, hello."), &[(1_000, 1_600, "hello")]));
        // Different words: a real reply.
        assert!(!is_echo(&(1_400, 2_400, "Yes, Thursday works"), &[(1_000, 1_600, "Does Thursday work")]));
        // Same words but long after: someone else saying it.
        assert!(!is_echo(&(9_000, 9_500, "hello"), &[(1_000, 1_600, "hello")]));
        // Them first (or at the same moment): that's bleed territory, not echo.
        assert!(!is_echo(&(1_000, 1_600, "hello"), &[(1_100, 1_600, "hello")]));
    }

    #[test]
    fn bleed_needs_them_not_clearly_after_me() {
        let them_first = [(1_000, 2_200, "we ship on thursday")];
        assert!(is_bleed(&(1_050, 2_200, "We ship on Thursday."), &them_first));
        // Them started 400 ms after Me: Teams echoing me, so keep the Me line.
        let echo = [(1_400, 2_600, "we ship on thursday")];
        assert!(!is_bleed(&(1_000, 2_200, "We ship on Thursday."), &echo));
    }

    #[test]
    fn similarity_identical_is_1_and_disjoint_is_0() {
        assert_eq!(text_similarity("We ship on Thursday.", "we ship on thursday"), 1.0);
        assert_eq!(text_similarity("apples and pears", "trains planes"), 0.0);
        assert_eq!(text_similarity("", ""), 0.0);
        let s = text_similarity("we ship on thursday", "we ship thursday");
        assert!((s - 6.0 / 7.0).abs() < 1e-6, "{s}");
    }

    #[test]
    fn bleed_requires_time_overlap() {
        let them = [(10_000u64, 12_000u64, "the rollout moves to thursday")];
        assert!(is_bleed(&(10_200, 12_100, "the rollout moves to thursday"), &them));
        assert!(!is_bleed(&(13_000, 15_000, "the rollout moves to thursday"), &them));
    }

    #[test]
    fn bleed_threshold_080() {
        let them = [(0u64, 5_000u64, "one two three four five")];
        // 4 of 5 words common: 2*4/(5+4) = 0.888 → bleed.
        assert!(is_bleed(&(0, 5_000, "one two three four"), &them));
        // 3 common of 5 vs 5: 0.6 → not bleed.
        assert!(!is_bleed(&(0, 5_000, "one two three six seven"), &them));
    }

    #[test]
    fn gate_blocks_overlap_with_300ms_margin() {
        let mut g = DictationGate::default();
        g.begin(10_000);
        g.end(12_000);
        assert!(g.blocks(9_000, 9_800), "within 300 ms before");
        assert!(g.blocks(11_000, 11_500));
        assert!(g.blocks(12_200, 13_000), "within 300 ms after");
    }

    #[test]
    fn gate_allows_after_margin() {
        let mut g = DictationGate::default();
        g.begin(10_000);
        g.end(12_000);
        assert!(!g.blocks(12_400, 14_000));
        assert!(!g.blocks(5_000, 9_600));
    }

    #[test]
    fn gate_open_window_blocks_everything_after_begin() {
        let mut g = DictationGate::default();
        g.begin(10_000);
        assert!(g.blocks(50_000, 51_000), "dictation still running");
        g.end(52_000);
        assert!(!g.blocks(60_000, 61_000));
    }
}
