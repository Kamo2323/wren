//! Speaker names for "Them" lines (meetings spec §5.3, plan Task 3). Pure.
//!
//! The Teams watcher (Task 4) records what Teams shows every ~250 ms: who is marked as speaking, the
//! roster, the user's own entry and mute state, and the window title. A Them segment takes the name with
//! the most speaking-overlap with the segment's window, shifted by Teams' highlight lag. The user's own
//! Teams name is never given to a Them line.

/// What one read of the Teams meeting window showed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TeamsObservation {
    pub at_ms: u64,
    pub speaking: Vec<String>,
    pub roster: Vec<String>,
    pub self_name: Option<String>,
    pub self_muted: Option<bool>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Attribution {
    pub name: Option<String>,
    pub confidence: f32,
}

/// Below this, a name is not given at all ("Speaker").
pub const MIN_CONFIDENCE: f32 = 0.35;
/// Below this the UI shows the name greyed with "?" (spec §5.3).
pub const LOW_CONFIDENCE: f32 = 0.6;
/// Default delay between audio and Teams lighting the speaker's tile (Granola's Teams value).
pub const TEAMS_LAG_MS: u64 = 900;
/// One observation never counts for longer than this (the watcher may stall).
const MAX_OBSERVATION_MS: u64 = 1_000;
/// Privacy first: a clip with at least this much time muted in Teams counts as muted (left out of notes).
const MUTED_MIN_MS: u64 = 500;
/// Observations are kept for this long (attribution only looks seconds back); bounds memory.
const KEEP_OBSERVATIONS_MS: u64 = 600_000;
const PRUNE_ABOVE: usize = 4_000;

/// `"Sarah Jones (You)"` → `"Sarah Jones"`; also `(Me)`. Case is kept.
pub fn strip_self_suffix(name: &str) -> &str {
    let t = name.trim();
    for suffix in ["(You)", "(you)", "(Me)", "(me)"] {
        if let Some(rest) = t.strip_suffix(suffix) {
            return rest.trim_end();
        }
    }
    t
}

fn is_self_marked(name: &str) -> bool {
    strip_self_suffix(name).len() != name.trim().len()
}

#[derive(Debug, Default, Clone)]
pub struct SpeakerTimeline {
    obs: Vec<TeamsObservation>,
    self_name: Option<String>,
    participants: Vec<String>,
    title: Option<String>,
}

impl SpeakerTimeline {
    fn is_self(&self, name: &str) -> bool {
        is_self_marked(name)
            || self.self_name.as_deref().is_some_and(|s| strip_self_suffix(name).eq_ignore_ascii_case(s))
    }

    pub fn record(&mut self, o: &TeamsObservation) {
        if let Some(s) = &o.self_name {
            self.self_name = Some(strip_self_suffix(s).to_string());
        }
        if o.title.is_some() {
            self.title = o.title.clone();
        }
        for n in o.roster.iter().chain(&o.speaking) {
            let clean = strip_self_suffix(n);
            if clean.is_empty() || self.is_self(n) {
                continue;
            }
            if !self.participants.iter().any(|p| p.eq_ignore_ascii_case(clean)) {
                self.participants.push(clean.to_string());
            }
        }
        let pos = self.obs.partition_point(|x| x.at_ms <= o.at_ms);
        self.obs.insert(pos, o.clone());
        if self.obs.len() > PRUNE_ABOVE {
            let latest = self.obs.last().map_or(0, |x| x.at_ms);
            let cut = self.obs.partition_point(|x| x.at_ms + KEEP_OBSERVATIONS_MS < latest);
            self.obs.drain(..cut);
        }
    }

    /// Observations held (tests).
    pub fn len(&self) -> usize {
        self.obs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.obs.is_empty()
    }

    /// `[from, to)` of observation `i`.
    fn interval(&self, i: usize) -> (u64, u64) {
        let at = self.obs[i].at_ms;
        let next = self.obs.get(i + 1).map_or(u64::MAX, |n| n.at_ms);
        (at, next.min(at + MAX_OBSERVATION_MS))
    }

    /// Observations whose interval can touch `[from, to)`, with the overlap length.
    fn overlapping(&self, from: u64, to: u64) -> impl Iterator<Item = (&TeamsObservation, u64)> + '_ {
        let first = self.obs.partition_point(|x| x.at_ms <= from).saturating_sub(1);
        (first..self.obs.len()).take_while(move |&i| self.obs[i].at_ms < to).filter_map(move |i| {
            let (a, b) = self.interval(i);
            let ov = b.min(to).saturating_sub(a.max(from));
            (ov > 0).then_some((&self.obs[i], ov))
        })
    }

    /// Best name for audio `[start_ms, end_ms)`; the Teams highlight is expected `lag_ms` later.
    pub fn attribute(&self, start_ms: u64, end_ms: u64, lag_ms: u64) -> Attribution {
        let (from, to) = (start_ms + lag_ms, end_ms + lag_ms);
        let none = Attribution { name: None, confidence: 0.0 };
        if to <= from {
            return none;
        }
        let mut totals: Vec<(String, u64)> = Vec::new();
        for (o, ov) in self.overlapping(from, to) {
            for n in &o.speaking {
                if self.is_self(n) {
                    continue;
                }
                let clean = strip_self_suffix(n);
                match totals.iter_mut().find(|(k, _)| k == clean) {
                    Some((_, t)) => *t += ov,
                    None => totals.push((clean.to_string(), ov)),
                }
            }
        }
        let Some((name, best)) = totals.into_iter().max_by_key(|(_, t)| *t) else { return none };
        let confidence = best as f32 / (to - from) as f32;
        if confidence < MIN_CONFIDENCE {
            return Attribution { name: None, confidence };
        }
        Attribution { name: Some(name), confidence: confidence.min(1.0) }
    }

    /// Whether the user was muted in Teams during `[start_ms, end_ms)` — for at least `MUTED_MIN_MS` or
    /// most of it (a private aside inside a longer clip keeps the clip out of the notes); `None` if never
    /// observed.
    pub fn muted_during(&self, start_ms: u64, end_ms: u64) -> Option<bool> {
        if end_ms <= start_ms {
            return None;
        }
        let (mut known, mut muted) = (0u64, 0u64);
        for (o, ov) in self.overlapping(start_ms, end_ms) {
            if let Some(m) = o.self_muted {
                known += ov;
                if m {
                    muted += ov;
                }
            }
        }
        (known > 0).then(|| muted >= MUTED_MIN_MS || muted * 2 > end_ms - start_ms)
    }

    /// Everyone seen in the roster or speaking, minus the user, in first-seen order.
    pub fn participants(&self) -> Vec<String> {
        let me = self.self_name.clone();
        self.participants
            .iter()
            .filter(|p| me.as_deref().is_none_or(|m| !p.eq_ignore_ascii_case(m)))
            .cloned()
            .collect()
    }

    pub fn self_name(&self) -> Option<String> {
        self.self_name.clone()
    }

    pub fn latest_title(&self) -> Option<String> {
        self.title.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(at: u64, speaking: &[&str]) -> TeamsObservation {
        TeamsObservation {
            at_ms: at,
            speaking: speaking.iter().map(|s| s.to_string()).collect(),
            roster: vec!["Sarah Jones".into(), "Tom Reid".into(), "Me Teams (You)".into()],
            self_name: Some("Me Teams".into()),
            self_muted: None,
            title: None,
        }
    }
    /// Observations every 250 ms from `from` to `to` with the same speakers.
    fn span(t: &mut SpeakerTimeline, from: u64, to: u64, speaking: &[&str]) {
        let mut at = from;
        while at < to {
            t.record(&obs(at, speaking));
            at += 250;
        }
    }

    #[test]
    fn single_speaker_full_overlap_conf_1() {
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 10_000, &["Sarah Jones"]);
        let a = t.attribute(2_000, 4_000, 0);
        assert_eq!(a.name.as_deref(), Some("Sarah Jones"));
        assert!((a.confidence - 1.0).abs() < 1e-6, "{}", a.confidence);
    }

    #[test]
    fn lag_shift_900ms_applied() {
        // Teams lights the tile ~900 ms after the audio: audio 1000–3000 ↔ highlight 1900–3900.
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 1_900, &[]);
        span(&mut t, 1_900, 3_900, &["Tom Reid"]);
        span(&mut t, 3_900, 8_000, &["Sarah Jones"]);
        let a = t.attribute(1_000, 3_000, 900);
        assert_eq!(a.name.as_deref(), Some("Tom Reid"));
        assert!(a.confidence > 0.9, "{}", a.confidence);
        let unshifted = t.attribute(1_000, 3_000, 0);
        assert!(unshifted.confidence < a.confidence);
    }

    #[test]
    fn two_speakers_more_overlap_wins() {
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 1_000, &["Tom Reid"]);
        span(&mut t, 1_000, 4_000, &["Sarah Jones"]);
        let a = t.attribute(0, 4_000, 0);
        assert_eq!(a.name.as_deref(), Some("Sarah Jones"));
        assert!((a.confidence - 0.75).abs() < 0.02, "{}", a.confidence);
    }

    #[test]
    fn self_name_never_attributed() {
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 5_000, &["Me Teams"]);
        assert_eq!(t.attribute(0, 5_000, 0).name, None);
        // Teams may also show it with the "(You)" suffix.
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 5_000, &["Me Teams (You)"]);
        assert_eq!(t.attribute(0, 5_000, 0).name, None);
    }

    #[test]
    fn no_observations_gives_none() {
        let t = SpeakerTimeline::default();
        let a = t.attribute(0, 1_000, 900);
        assert_eq!(a.name, None);
        assert_eq!(a.confidence, 0.0);
    }

    #[test]
    fn below_035_gives_none() {
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 1_000, &["Tom Reid"]);
        span(&mut t, 1_000, 4_000, &[]);
        assert_eq!(t.attribute(0, 4_000, 0).name, None, "25 % overlap is too little");
    }

    #[test]
    fn gaps_between_observations_are_capped_at_1s() {
        let mut t = SpeakerTimeline::default();
        t.record(&obs(0, &["Tom Reid"]));
        t.record(&obs(10_000, &[]));
        // Tom counts for at most 1 s, not the whole 10 s gap.
        let a = t.attribute(0, 4_000, 0);
        assert_eq!(a.name, None);
    }

    #[test]
    fn participants_exclude_self_and_dedupe() {
        let mut t = SpeakerTimeline::default();
        t.record(&obs(0, &["Sarah Jones"]));
        t.record(&obs(250, &["Guest Speaker", "Sarah Jones"]));
        assert_eq!(t.participants(), vec!["Sarah Jones", "Tom Reid", "Guest Speaker"]);
        assert_eq!(t.self_name().as_deref(), Some("Me Teams"));
    }

    #[test]
    fn muted_majority_true() {
        let mut t = SpeakerTimeline::default();
        for (at, m) in [(0, false), (1_000, true), (2_000, true), (3_000, true)] {
            let mut o = obs(at, &[]);
            o.self_muted = Some(m);
            t.record(&o);
        }
        assert_eq!(t.muted_during(1_000, 3_900), Some(true));
        assert_eq!(t.muted_during(0, 900), Some(false));
    }

    #[test]
    fn a_short_muted_aside_inside_a_long_clip_counts_as_muted() {
        let mut t = SpeakerTimeline::default();
        for (at, m) in [(0, false), (8_000, true)] {
            let mut o = obs(at, &[]);
            o.self_muted = Some(m);
            t.record(&o);
        }
        // 8 s unmuted then a 2 s private aside: the whole clip stays out of the notes.
        let mut o = obs(9_000, &[]);
        o.self_muted = Some(true);
        t.record(&o);
        assert_eq!(t.muted_during(0, 10_000), Some(true));
    }

    #[test]
    fn old_observations_are_pruned() {
        let mut t = SpeakerTimeline::default();
        for i in 0..30_000u64 {
            t.record(&obs(i * 250, &["Sarah Jones"]));
        }
        assert!(t.len() <= 4_000, "{}", t.len());
        assert_eq!(t.attribute(30_000 * 250 - 3_000, 30_000 * 250 - 1_000, 0).name.as_deref(), Some("Sarah Jones"));
        assert_eq!(t.participants(), vec!["Sarah Jones", "Tom Reid"], "participants survive pruning");
    }

    #[test]
    fn muted_unknown_none() {
        let mut t = SpeakerTimeline::default();
        span(&mut t, 0, 3_000, &["Sarah Jones"]);
        assert_eq!(t.muted_during(0, 2_000), None);
    }

    #[test]
    fn latest_title_kept() {
        let mut t = SpeakerTimeline::default();
        let mut o = obs(0, &[]);
        o.title = Some("Weekly sync".into());
        t.record(&o);
        t.record(&obs(250, &[]));
        assert_eq!(t.latest_title().as_deref(), Some("Weekly sync"));
    }
}
