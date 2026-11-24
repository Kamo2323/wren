//! Bleed-through removal and the dictation gate (meetings spec §5.1–5.2, plan Task 2). Pure.
//!
//! *Bleed*: on speakers without headphones, the mic hears the other people too, so a "Me" segment can
//! repeat a "Them" segment. A Me segment that overlaps a Them segment in time with ≥ 80 % word
//! similarity is dropped.
//!
//! *Dictation gate*: while the user dictates (hotkey held / hands-free), the meeting's Me stream must
//! not turn the dictated words into a meeting line, so Me segments overlapping a dictation window
//! (widened by 300 ms each side) are dropped.

pub const BLEED_SIMILARITY: f32 = 0.80;
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

/// `(start_ms, end_ms, text)` of a Me segment against the Them segments around it.
pub fn is_bleed(me: &(u64, u64, &str), them: &[(u64, u64, &str)]) -> bool {
    them.iter().any(|t| me.0 < t.1 && t.0 < me.1 && text_similarity(me.2, t.2) >= BLEED_SIMILARITY)
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
