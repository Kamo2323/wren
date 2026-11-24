use super::*;
use crate::meetings::claude::{ClaudeError, Completed, Usage};
use crate::meetings::{CaptureMode, Length, MeetingStatus, NewSegment, Source};
use crate::store::Store;
use std::sync::Arc;

fn store() -> (std::path::PathBuf, Arc<Store>) {
    let d = std::env::temp_dir().join(format!("wren-enh-{}", uuid::Uuid::new_v4()));
    let s = Arc::new(Store::open(&d.join("w.db")).unwrap());
    (d, s)
}

fn seg(m: &str, src: Source, who: Option<&str>, at: u64, text: &str, muted: bool) -> NewSegment {
    NewSegment {
        meeting_id: m.into(),
        source: src,
        speaker: who.map(str::to_string),
        confidence: 0.9,
        muted,
        start_ms: at,
        end_ms: at + 2_000,
        text: text.into(),
    }
}

fn meeting(s: &Store) -> String {
    let m = s.meeting_create("Weekly sync", CaptureMode::TeamsOnly).unwrap();
    s.meeting_set_status(&m.id, MeetingStatus::Ended, Some(chrono::Utc::now()), 120_000).unwrap();
    s.meeting_update(&m.id, None, None, Some(&["Sarah Jones".to_string()]), Some("Max Teams")).unwrap();
    s.segment_insert(&seg(&m.id, Source::Me, Some("Maximilian"), 5_000, "Morning all.", false)).unwrap();
    s.segment_insert(&seg(&m.id, Source::Me, Some("Maximilian"), 9_000, "private aside to my desk mate", true)).unwrap();
    s.segment_insert(&seg(&m.id, Source::Them, Some("Sarah Jones"), 72_000, "Rollout moves to Thursday.", false)).unwrap();
    s.segment_insert(&seg(&m.id, Source::Them, None, 3_725_000, "I'll chase QA.", false)).unwrap();
    m.id
}

fn ctx(s: &Store, id: &str) -> String {
    let m = s.meeting_get(id).unwrap().unwrap();
    context_block(&m, &s.segments(id).unwrap(), &tiptap_to_markdown(&m.my_notes), "Maximilian")
}

#[test]
fn muted_lines_excluded_from_context() {
    let (_d, s) = store();
    let id = meeting(&s);
    let c = ctx(&s, &id);
    assert!(c.contains("Morning all."));
    assert!(!c.contains("private aside"), "muted lines never reach Claude");
}

#[test]
fn transcript_lines_have_timestamps_and_names() {
    let (_d, s) = store();
    let id = meeting(&s);
    let c = ctx(&s, &id);
    assert!(c.contains("[00:05] Maximilian: Morning all."), "{c}");
    assert!(c.contains("[01:12] Sarah Jones: Rollout moves to Thursday."), "{c}");
    assert!(c.contains("[1:02:05] Speaker: I'll chase QA."), "hour format and unnamed lines: {c}");
    assert!(c.contains("Meeting: Weekly sync"));
    assert!(c.contains("Participants: Sarah Jones"));
}

#[test]
fn speaker_fallback_written_as_speaker() {
    let (_d, s) = store();
    let id = meeting(&s);
    assert!(ctx(&s, &id).contains("] Speaker: "));
    assert!(SYSTEM_PROMPT.contains("Speaker"), "the system prompt explains unnamed lines");
}

#[test]
fn self_alias_line_present_when_known() {
    let (_d, s) = store();
    let id = meeting(&s);
    assert!(ctx(&s, &id).contains("Me: Maximilian (also appears in Teams as \"Max Teams\")"));
}

#[test]
fn my_notes_converted_to_markdown() {
    let doc = r#"{"type":"doc","content":[
      {"type":"heading","attrs":{"level":2},"content":[{"type":"text","text":"Rollout"}]},
      {"type":"bulletList","content":[
        {"type":"listItem","content":[{"type":"paragraph","content":[
          {"type":"text","text":"ship "},{"type":"text","text":"Thursday","marks":[{"type":"bold"}]}]}]}]},
      {"type":"taskList","content":[
        {"type":"taskItem","attrs":{"checked":true},"content":[{"type":"paragraph","content":[{"type":"text","text":"email QA"}]}]},
        {"type":"taskItem","attrs":{"checked":false},"content":[{"type":"paragraph","content":[{"type":"text","text":"book room"}]}]}]},
      {"type":"orderedList","content":[
        {"type":"listItem","content":[{"type":"paragraph","content":[{"type":"text","text":"first"}]}]}]},
      {"type":"paragraph","content":[{"type":"text","text":"plain"}]}
    ]}"#;
    let md = tiptap_to_markdown(doc);
    assert_eq!(md, "## Rollout\n\n- ship **Thursday**\n\n- [x] email QA\n- [ ] book room\n\n1. first\n\nplain");
    assert_eq!(tiptap_to_markdown(""), "");
    assert_eq!(tiptap_to_markdown("just text"), "just text", "non-JSON notes pass through");
}

#[test]
fn empty_notes_say_none() {
    let (_d, s) = store();
    let id = meeting(&s);
    assert!(ctx(&s, &id).contains("My notes:\n(none)"));
}

#[test]
fn instructions_contain_required_headings() {
    let short = instruction(Length::Short);
    for h in ["## Summary", "## Key points", "## Action items"] {
        assert!(short.contains(h), "short: {h}");
    }
    let medium = instruction(Length::Medium);
    for h in ["## Summary", "## Topics discussed", "## Decisions", "## Action items", "## Open questions"] {
        assert!(medium.contains(h), "medium: {h}");
    }
    let long = instruction(Length::Long);
    for h in ["## Summary", "## Topics discussed", "## Decisions", "## Action items", "## Open questions", "## Walkthrough"] {
        assert!(long.contains(h), "long: {h}");
    }
    assert!(medium.contains("Owner — task — due"));
}

struct FakeWriter {
    fail_after: Option<usize>,
    deltas: Vec<&'static str>,
    model_seen: parking_lot::Mutex<String>,
}

impl NoteWriter for FakeWriter {
    fn write(&self, body: &serde_json::Value, on_delta: &mut dyn FnMut(&str)) -> Result<Completed, ClaudeError> {
        *self.model_seen.lock() = body["model"].as_str().unwrap_or("").to_string();
        for (i, d) in self.deltas.iter().enumerate() {
            if self.fail_after == Some(i) {
                return Err(ClaudeError::Network("the connection dropped".into()));
            }
            on_delta(d);
        }
        Ok(Completed {
            text: self.deltas.concat(),
            usage: Usage { input: 1_000, output: 200, cache_read: 0, cache_write: 15_000 },
            stop_reason: "end_turn".into(),
        })
    }
}

#[test]
fn failed_stream_persists_nothing() {
    let (_d, s) = store();
    let id = meeting(&s);
    let w = FakeWriter { fail_after: Some(2), deltas: vec!["## Summary\n", "Good ", "meeting"], model_seen: Default::default() };
    let mut got = Vec::new();
    let r = generate(&s, &w, "claude-opus-5-5", "Maximilian", &id, Length::Medium, &mut |e| got.push(e));
    assert_eq!(r.unwrap_err(), ClaudeError::Network("the connection dropped".into()));
    assert!(s.notes(&id).unwrap().is_empty(), "no partial note saved");
    assert_eq!(got.iter().filter(|e| matches!(e, NoteEvent::Delta { .. })).count(), 2);
    assert!(matches!(got.last(), Some(NoteEvent::Error { kind, .. }) if kind == "network"));
}

#[test]
fn failed_regeneration_keeps_the_previous_note() {
    let (_d, s) = store();
    let id = meeting(&s);
    let ok = FakeWriter { fail_after: None, deltas: vec!["## Summary\nfirst"], model_seen: Default::default() };
    generate(&s, &ok, "claude-opus-5-5", "Maximilian", &id, Length::Short, &mut |_| {}).unwrap();
    let bad = FakeWriter { fail_after: Some(0), deltas: vec!["x"], model_seen: Default::default() };
    let _ = generate(&s, &bad, "claude-opus-5-5", "Maximilian", &id, Length::Short, &mut |_| {});
    assert_eq!(s.notes(&id).unwrap()[0].markdown, "## Summary\nfirst");
}

#[test]
fn success_upserts_and_clears_stale() {
    let (_d, s) = store();
    let id = meeting(&s);
    let w = FakeWriter { fail_after: None, deltas: vec!["## Summary\n", "All good."], model_seen: Default::default() };
    generate(&s, &w, "claude-sonnet-5-5", "Maximilian", &id, Length::Medium, &mut |_| {}).unwrap();
    s.meeting_update(&id, None, Some("{\"type\":\"doc\",\"content\":[]}"), None, None).unwrap();
    assert!(s.notes(&id).unwrap()[0].stale);
    let mut got = Vec::new();
    let note = generate(&s, &w, "claude-sonnet-5-5", "Maximilian", &id, Length::Medium, &mut |e| got.push(e)).unwrap();
    assert_eq!(note.markdown, "## Summary\nAll good.");
    assert_eq!(note.model, "claude-sonnet-5-5");
    assert_eq!(*w.model_seen.lock(), "claude-sonnet-5-5");
    assert_eq!(note.cache_write_tokens, 15_000);
    assert!(note.cost_usd > 0.0);
    let saved = &s.notes(&id).unwrap()[0];
    assert!(!saved.stale);
    assert!(matches!(got.last(), Some(NoteEvent::Done { .. })));
}

#[test]
fn nothing_to_summarise_is_an_error_without_a_call() {
    let (_d, s) = store();
    let m = s.meeting_create("Empty", CaptureMode::MicOnly).unwrap();
    let w = FakeWriter { fail_after: Some(0), deltas: vec![], model_seen: Default::default() };
    let r = generate(&s, &w, "claude-opus-5-5", "Maximilian", &m.id, Length::Medium, &mut |_| {});
    assert!(matches!(r, Err(ClaudeError::Api(_))));
    assert_eq!(*w.model_seen.lock(), "", "no request was made");
}
