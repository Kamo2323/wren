//! Speaker-naming replay benchmark (pure; no audio, no engine).
//!
//! Feeds the Teams readings from a call diagnostics file (`meetings::diagnostics`) through the real
//! `SpeakerTimeline` — the same `record` + `attribute` calls the meeting worker makes — for each Them
//! line of a *case*, and scores the names against an answer key. Run it with
//! `cargo run --example speaker_replay` (see `docs/meetings-speaker-test-call.md`).
//!
//! ## Case file (JSON)
//!
//! ```json
//! {
//!   "name": "five-person",
//!   "lagMs": 900,
//!   "participants": ["Tom Reid", "Sarah Jones"],
//!   "lines": [
//!     { "startMs": 1000, "endMs": 5000, "text": "…", "expected": "Tom Reid" },
//!     { "startMs": 22000, "endMs": 27500, "expected": "Tom Reid", "alsoSpeaking": ["Sarah Jones"] },
//!     { "startMs": 60000, "endMs": 61000, "expected": null }
//!   ]
//! }
//! ```
//!
//! - `lines` are the Them lines (transcript segments) with their audio-clock times in ms.
//! - `expected` is the right name, spelled as Teams shows it (compared ignoring case). For a line where
//!   more than one person spoke, it is whoever said most of it, and `alsoSpeaking` lists the others
//!   (the line is reported as "mixed"). `null` or missing: not scored (noise, a line you can't place).
//! - `lagMs` (optional) overrides the Teams highlight lag; the default is the app's `TEAMS_LAG_MS`.
//! - `name`, `text` and `participants` are informational.

use crate::meetings::diagnostics::Recorded;
use crate::meetings::speakers::{strip_self_suffix, SpeakerTimeline, TeamsObservation, LOW_CONFIDENCE, TEAMS_LAG_MS};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Case {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lag_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    pub lines: Vec<CaseLine>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseLine {
    pub start_ms: u64,
    pub end_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_speaking: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Correct,
    /// A name was given, but the wrong one.
    WrongName,
    /// No name was given ("Speaker") although someone was expected.
    Unnamed,
    /// No answer in the key: not scored.
    NotScored,
}

#[derive(Debug, Clone)]
pub struct LineResult {
    pub start_ms: u64,
    pub end_ms: u64,
    pub expected: Option<String>,
    pub named: Option<String>,
    pub confidence: f32,
    pub outcome: Outcome,
    pub mixed: bool,
    /// Who Teams highlighted during the line's (lag-shifted) window, in order, repeats collapsed.
    pub teams_showed: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub name: String,
    pub lag_ms: u64,
    pub lines: Vec<LineResult>,
}

impl Report {
    pub fn count(&self, o: Outcome) -> usize {
        self.lines.iter().filter(|l| l.outcome == o).count()
    }
    pub fn correct(&self) -> usize {
        self.count(Outcome::Correct)
    }
    pub fn scored(&self) -> usize {
        self.lines.len() - self.count(Outcome::NotScored)
    }
    pub fn mixed(&self) -> usize {
        self.lines.iter().filter(|l| l.mixed).count()
    }
    /// Percentage of scored lines named correctly (0 when nothing is scored).
    pub fn percent(&self) -> f64 {
        if self.scored() == 0 { 0.0 } else { self.correct() as f64 * 100.0 / self.scored() as f64 }
    }
    /// `"N of M lines named correctly, X%"`.
    pub fn score_line(&self) -> String {
        format!("{} of {} lines named correctly, {:.1}%", self.correct(), self.scored(), self.percent())
    }
}

fn same_name(a: &str, b: &str) -> bool {
    strip_self_suffix(a).eq_ignore_ascii_case(strip_self_suffix(b))
}

/// The timeline the live meeting would have built from these readings.
pub fn timeline(readings: &[TeamsObservation]) -> SpeakerTimeline {
    let mut t = SpeakerTimeline::default();
    for o in readings {
        t.record(o);
    }
    t
}

fn teams_showed(readings: &[TeamsObservation], from: u64, to: u64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let first = readings.partition_point(|o| o.at_ms <= from).saturating_sub(1);
    for o in readings[first..].iter().take_while(|o| o.at_ms < to) {
        let label = if o.speaking.is_empty() { "-".to_string() } else { o.speaking.join("+") };
        if out.last() != Some(&label) {
            out.push(label);
        }
    }
    out
}

/// Score `case` against `readings`. `lag_override` beats the case's `lagMs`, which beats the default.
pub fn run(readings: &[TeamsObservation], case: &Case, lag_override: Option<u64>) -> Report {
    let lag_ms = lag_override.or(case.lag_ms).unwrap_or(TEAMS_LAG_MS);
    let mut sorted = readings.to_vec();
    sorted.sort_by_key(|o| o.at_ms);
    let t = timeline(&sorted);
    let lines = case
        .lines
        .iter()
        .map(|l| {
            let a = t.attribute(l.start_ms, l.end_ms, lag_ms);
            let expected = l.expected.as_ref().map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
            let outcome = match (&expected, &a.name) {
                (None, _) => Outcome::NotScored,
                (Some(_), None) => Outcome::Unnamed,
                (Some(e), Some(n)) if same_name(e, n) => Outcome::Correct,
                (Some(_), Some(_)) => Outcome::WrongName,
            };
            LineResult {
                start_ms: l.start_ms,
                end_ms: l.end_ms,
                expected,
                named: a.name,
                confidence: a.confidence,
                outcome,
                mixed: !l.also_speaking.is_empty(),
                teams_showed: teams_showed(&sorted, l.start_ms + lag_ms, l.end_ms + lag_ms),
            }
        })
        .collect();
    Report { name: case.name.clone().unwrap_or_else(|| "case".into()), lag_ms, lines }
}

/// A case skeleton from a recorded call: its Them lines with times and text, answers left empty, plus
/// the names Teams showed (to copy into `expected`).
pub fn skeleton(rec: &Recorded) -> Case {
    let t = timeline(&rec.readings);
    Case {
        name: Some(if rec.header.meeting_id.is_empty() { "recorded call".into() } else { rec.header.meeting_id.clone() }),
        lag_ms: None,
        participants: t.participants(),
        lines: rec
            .lines
            .iter()
            .filter(|l| l.source == "them")
            .map(|l| CaseLine {
                start_ms: l.start_ms,
                end_ms: l.end_ms,
                text: Some(l.text.clone()),
                expected: None,
                also_speaking: Vec::new(),
            })
            .collect(),
    }
}

fn secs(ms: u64) -> String {
    format!("{:.1}", ms as f64 / 1000.0)
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Case: {} (Teams lag {} ms)", self.name, self.lag_ms)?;
        writeln!(f, "{:>3}  {:>13}  {:<20} {:<20} {:>5}  {:<8} Teams showed", "#", "audio (s)", "expected", "named", "conf", "result")?;
        for (i, l) in self.lines.iter().enumerate() {
            let named = match &l.named {
                Some(n) if l.confidence < LOW_CONFIDENCE => format!("{n} ?"),
                Some(n) => n.clone(),
                None => "Speaker".into(),
            };
            let result = match l.outcome {
                Outcome::Correct => "ok",
                Outcome::WrongName => "WRONG",
                Outcome::Unnamed => "unnamed",
                Outcome::NotScored => "-",
            };
            writeln!(
                f,
                "{:>3}  {:>13}  {:<20} {:<20} {:>5.2}  {:<8} {}{}",
                i + 1,
                format!("{}-{}", secs(l.start_ms), secs(l.end_ms)),
                clip(l.expected.as_deref().unwrap_or("-"), 20),
                clip(&named, 20),
                l.confidence,
                result,
                l.teams_showed.join(" > "),
                if l.mixed { "  [mixed]" } else { "" },
            )?;
        }
        writeln!(
            f,
            "Score: {} ({} wrong name, {} unnamed; {} mixed lines; {} not scored)",
            self.score_line(),
            self.count(Outcome::WrongName),
            self.count(Outcome::Unnamed),
            self.mixed(),
            self.count(Outcome::NotScored),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::diagnostics::parse;

    macro_rules! fixture {
        ($name:literal) => {{
            let diag = include_str!(concat!("../../tests/fixtures/speaker-replay/", $name, "/call-diagnostics.jsonl"));
            let case = include_str!(concat!("../../tests/fixtures/speaker-replay/", $name, "/case.json"));
            (parse(diag).unwrap(), serde_json::from_str::<Case>(case).unwrap())
        }};
    }

    #[test]
    fn one_person_fixture_scores_every_line() {
        let (rec, case) = fixture!("one-person");
        let r = run(&rec.readings, &case, None);
        println!("{r}");
        assert_eq!((r.correct(), r.scored()), (6, 6), "{r}");
    }

    #[test]
    fn five_person_fixture_baseline() {
        // Today's attribution on the synthetic five-person call: the fast back-and-forth and overlap
        // lines still get their majority speaker (the minority's words are credited to them), and the
        // stretch where Teams shows nobody comes out as "Speaker". A change to `speakers.rs` that moves
        // these numbers should update them on purpose.
        let (rec, case) = fixture!("five-person");
        let r = run(&rec.readings, &case, None);
        println!("{r}");
        assert_eq!(r.scored(), 15, "{r}");
        assert_eq!(r.mixed(), 4, "{r}");
        assert_eq!(r.count(Outcome::Unnamed), 1, "{r}");
        assert_eq!(r.count(Outcome::WrongName), 0, "{r}");
        assert_eq!(r.correct(), 14, "{r}");
        assert_eq!(r.score_line(), "14 of 15 lines named correctly, 93.3%");
    }

    #[test]
    fn lag_matters_on_fast_turns() {
        // With no lag correction, short turns pick up the previous speaker's highlight.
        let (rec, case) = fixture!("five-person");
        let r = run(&rec.readings, &case, Some(0));
        assert!(r.correct() < 14, "{r}");
    }

    #[test]
    fn replay_matches_the_live_attribution_call() {
        let (rec, case) = fixture!("five-person");
        let t = timeline(&rec.readings);
        let r = run(&rec.readings, &case, None);
        for (l, c) in r.lines.iter().zip(&case.lines) {
            let a = t.attribute(c.start_ms, c.end_ms, TEAMS_LAG_MS);
            assert_eq!((&l.named, l.confidence), (&a.name, a.confidence));
        }
    }

    #[test]
    fn outcomes_and_names_ignore_case_and_you_suffix() {
        let readings: Vec<TeamsObservation> = (0..20)
            .map(|i| TeamsObservation { at_ms: i * 250, speaking: vec!["Tom Reid".into()], ..Default::default() })
            .collect();
        let line = |exp: Option<&str>| CaseLine { start_ms: 0, end_ms: 2_000, expected: exp.map(str::to_string), ..Default::default() };
        let case = Case { lines: vec![line(Some("tom reid")), line(Some("Sarah")), line(None)], ..Default::default() };
        let r = run(&readings, &case, Some(0));
        let o: Vec<Outcome> = r.lines.iter().map(|l| l.outcome).collect();
        assert_eq!(o, vec![Outcome::Correct, Outcome::WrongName, Outcome::NotScored]);
        assert_eq!(r.score_line(), "1 of 2 lines named correctly, 50.0%");
        let empty = run(&[], &Case { lines: vec![line(Some("Tom Reid"))], ..Default::default() }, None);
        assert_eq!(empty.lines[0].outcome, Outcome::Unnamed);
    }

    #[test]
    fn skeleton_lists_them_lines_and_participants() {
        let (rec, _) = fixture!("five-person");
        let s = skeleton(&rec);
        assert_eq!(s.lines.len(), rec.lines.iter().filter(|l| l.source == "them").count());
        assert!(s.lines.iter().all(|l| l.expected.is_none() && l.text.is_some()));
        assert!(s.participants.iter().any(|p| p == "Tom Reid"));
        assert!(!s.participants.iter().any(|p| p.starts_with("Maximilian")), "the user is left out");
    }
}
