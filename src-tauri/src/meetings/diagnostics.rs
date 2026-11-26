//! "Save call diagnostics" (Settings → Meeting notes, off by default).
//!
//! While on, a meeting writes `call-diagnostics.jsonl` next to its `me.wav` / `them.wav`: every
//! Teams reading the watcher made (who was highlighted as speaking, the roster, the user's own name
//! and mute state, the window title) and every transcript line Wren saved, all on the meeting's audio
//! clock (milliseconds since the meeting started, pauses included — the same clock as the WAVs and
//! the saved lines). The replay benchmark (`meetings::replay`, `examples/speaker_replay.rs`) feeds the
//! readings back through the real speaker attribution to score it.
//!
//! The file holds people's names and what was said, so it stays on this computer and goes with the
//! audio: the 24-hour audio sweep and deleting the meeting both remove it (`store::meetings`).
//!
//! ## File format (JSON Lines, one object per line, `"format": 1`)
//!
//! ```text
//! {"kind":"header","format":1,"meetingId":"…","startedAtUtc":"…","teamsLagMs":900,"platform":"macos"}
//! {"kind":"teams","atMs":250,"speaking":["Tom Reid"],"selfMuted":false,"roster":["Tom Reid","Sarah Jones"],"selfName":"Max","title":"Weekly sync"}
//! {"kind":"teams","atMs":500,"speaking":[],"selfMuted":false}
//! {"kind":"line","source":"them","startMs":1000,"endMs":4200,"speaker":"Tom Reid","confidence":0.92,"muted":false,"text":"…"}
//! ```
//!
//! - `teams`: `speaking` is always written (`[]` = Teams highlighted nobody). `selfMuted` is written
//!   when the read could see it. `roster`, `selfName` and `title` are written only when they differ
//!   from the previous reading (absent = unchanged; `null` = no longer seen). Readings are in time order.
//! - `line`: one per saved transcript line, written when it is saved (so not in time order). `source`
//!   is `"me"` or `"them"`; `speaker` is `null` for an unnamed Them line ("Speaker").

use crate::meetings::speakers::TeamsObservation;
use serde_json::{json, Map, Value};
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Name of the diagnostics file inside a meeting's folder (deleted with the audio).
pub const FILE_NAME: &str = "call-diagnostics.jsonl";
pub const FORMAT: u64 = 1;

/// Appends readings and lines to a meeting's diagnostics file. Every record is one `write_all`, so a
/// crash loses at most the line being written.
pub struct Writer {
    file: File,
    last: Option<TeamsObservation>,
}

impl Writer {
    /// Create (or replace) the file in `dir` and write its header.
    pub fn create(dir: &Path, meeting_id: &str, started_at_utc: &str, teams_lag_ms: u64) -> std::io::Result<Self> {
        let mut w = Writer { file: File::create(dir.join(FILE_NAME))?, last: None };
        w.put(&json!({
            "kind": "header",
            "format": FORMAT,
            "meetingId": meeting_id,
            "startedAtUtc": started_at_utc,
            "teamsLagMs": teams_lag_ms,
            "platform": std::env::consts::OS,
        }))?;
        Ok(w)
    }

    fn put(&mut self, v: &Value) -> std::io::Result<()> {
        let mut line = v.to_string();
        line.push('\n');
        self.file.write_all(line.as_bytes())
    }

    pub fn reading(&mut self, o: &TeamsObservation) -> std::io::Result<()> {
        let mut m = Map::new();
        m.insert("kind".into(), "teams".into());
        m.insert("atMs".into(), o.at_ms.into());
        m.insert("speaking".into(), json!(o.speaking));
        if let Some(muted) = o.self_muted {
            m.insert("selfMuted".into(), muted.into());
        }
        let prev = self.last.as_ref();
        if prev.is_none_or(|p| p.roster != o.roster) {
            m.insert("roster".into(), json!(o.roster));
        }
        if prev.is_none_or(|p| p.self_name != o.self_name) {
            m.insert("selfName".into(), json!(o.self_name));
        }
        if prev.is_none_or(|p| p.title != o.title) {
            m.insert("title".into(), json!(o.title));
        }
        self.put(&Value::Object(m))?;
        self.last = Some(o.clone());
        Ok(())
    }

    pub fn line(&mut self, l: &RecordedLine) -> std::io::Result<()> {
        self.put(&json!({
            "kind": "line",
            "source": l.source,
            "startMs": l.start_ms,
            "endMs": l.end_ms,
            "speaker": l.speaker,
            "confidence": l.confidence,
            "muted": l.muted,
            "text": l.text,
        }))
    }
}

/// A transcript line as saved during the meeting.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedLine {
    /// `"me"` or `"them"`.
    pub source: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: Option<String>,
    pub confidence: f32,
    pub muted: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    pub meeting_id: String,
    pub started_at_utc: String,
    pub teams_lag_ms: Option<u64>,
    pub platform: String,
}

/// A diagnostics file read back.
#[derive(Debug, Clone, Default)]
pub struct Recorded {
    pub header: Header,
    /// Every Teams reading, in time order, exactly as the speaker timeline received it.
    pub readings: Vec<TeamsObservation>,
    /// Saved lines, sorted by start time.
    pub lines: Vec<RecordedLine>,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Parse a diagnostics file. Unknown kinds are skipped (newer files stay readable); a malformed line
/// is an error naming its line number.
pub fn parse(text: &str) -> Result<Recorded, String> {
    let mut out = Recorded::default();
    let mut prev = TeamsObservation::default();
    for (i, raw) in text.lines().enumerate() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let bad = |what: &str| format!("line {}: {what}", i + 1);
        let v: Value = serde_json::from_str(raw).map_err(|_| bad("not JSON"))?;
        match v.get("kind").and_then(Value::as_str) {
            Some("header") => {
                let format = v.get("format").and_then(Value::as_u64).unwrap_or(0);
                if format > FORMAT {
                    return Err(bad(&format!("format {format} is newer than this reader ({FORMAT})")));
                }
                out.header = Header {
                    meeting_id: v.get("meetingId").and_then(Value::as_str).unwrap_or_default().to_string(),
                    started_at_utc: v.get("startedAtUtc").and_then(Value::as_str).unwrap_or_default().to_string(),
                    teams_lag_ms: v.get("teamsLagMs").and_then(Value::as_u64),
                    platform: v.get("platform").and_then(Value::as_str).unwrap_or_default().to_string(),
                };
            }
            Some("teams") => {
                let at_ms = v.get("atMs").and_then(Value::as_u64).ok_or_else(|| bad("teams reading without atMs"))?;
                let mut o = TeamsObservation {
                    at_ms,
                    speaking: strings(v.get("speaking").unwrap_or(&Value::Null)),
                    roster: prev.roster.clone(),
                    self_name: prev.self_name.clone(),
                    self_muted: v.get("selfMuted").and_then(Value::as_bool),
                    title: prev.title.clone(),
                };
                if let Some(r) = v.get("roster") {
                    o.roster = strings(r);
                }
                if let Some(n) = v.get("selfName") {
                    o.self_name = n.as_str().map(str::to_string);
                }
                if let Some(t) = v.get("title") {
                    o.title = t.as_str().map(str::to_string);
                }
                prev = o.clone();
                out.readings.push(o);
            }
            Some("line") => {
                let num = |k: &str| v.get(k).and_then(Value::as_u64).ok_or_else(|| bad(&format!("line without {k}")));
                out.lines.push(RecordedLine {
                    source: v.get("source").and_then(Value::as_str).unwrap_or("them").to_string(),
                    start_ms: num("startMs")?,
                    end_ms: num("endMs")?,
                    speaker: v.get("speaker").and_then(Value::as_str).map(str::to_string),
                    confidence: v.get("confidence").and_then(Value::as_f64).unwrap_or(0.0) as f32,
                    muted: v.get("muted").and_then(Value::as_bool).unwrap_or(false),
                    text: v.get("text").and_then(Value::as_str).unwrap_or_default().to_string(),
                });
            }
            _ => {}
        }
    }
    out.readings.sort_by_key(|o| o.at_ms);
    out.lines.sort_by_key(|l| (l.start_ms, l.end_ms));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wren-diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn reading(at: u64, speaking: &[&str], roster: &[&str], muted: Option<bool>, title: Option<&str>) -> TeamsObservation {
        TeamsObservation {
            at_ms: at,
            speaking: speaking.iter().map(|s| s.to_string()).collect(),
            roster: roster.iter().map(|s| s.to_string()).collect(),
            self_name: Some("Max".into()),
            self_muted: muted,
            title: title.map(str::to_string),
        }
    }

    #[test]
    fn readings_and_lines_round_trip_losslessly() {
        let dir = temp_dir();
        let readings = vec![
            reading(0, &[], &["Tom Reid", "Max (You)"], Some(false), Some("Sync")),
            reading(250, &["Tom Reid"], &["Tom Reid", "Max (You)"], Some(false), Some("Sync")),
            // Roster grows, title disappears, mute unreadable.
            reading(500, &["Tom Reid", "Sarah Jones"], &["Tom Reid", "Sarah Jones", "Max (You)"], None, None),
            // Roster empty (Teams hid it): must replay as empty, not as the previous roster.
            reading(750, &[], &[], Some(true), None),
        ];
        let mut w = Writer::create(&dir, "m1", "2026-11-26T10:00:00Z", 900).unwrap();
        for r in &readings {
            w.reading(r).unwrap();
        }
        let line = RecordedLine {
            source: "them".into(),
            start_ms: 100,
            end_ms: 600,
            speaker: Some("Tom Reid".into()),
            confidence: 0.75,
            muted: false,
            text: "Hello there.".into(),
        };
        w.line(&line).unwrap();
        w.line(&RecordedLine { speaker: None, start_ms: 0, ..line.clone() }).unwrap();
        drop(w);
        let text = std::fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        // Unchanged roster/title/selfName are not repeated.
        assert!(!text.lines().nth(2).unwrap().contains("roster"), "{text}");
        let r = parse(&text).unwrap();
        assert_eq!(r.readings, readings);
        assert_eq!(r.header.meeting_id, "m1");
        assert_eq!(r.header.teams_lag_ms, Some(900));
        assert_eq!(r.lines.len(), 2);
        assert_eq!(r.lines[0].speaker, None, "sorted by start");
        assert_eq!(r.lines[1], line);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_lines_name_their_line_number() {
        let err = parse("{\"kind\":\"header\",\"format\":1}\nnot json\n").unwrap_err();
        assert!(err.starts_with("line 2"), "{err}");
        assert!(parse("{\"kind\":\"header\",\"format\":99}").is_err(), "newer format refused");
        assert!(parse("{\"kind\":\"future-thing\"}\n").is_ok(), "unknown kinds skipped");
    }
}
