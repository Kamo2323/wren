//! Enhanced notes (meetings spec §5.4, plan Task 10): build the prompt from the meeting, the user's
//! notes and the named transcript, stream Claude's answer to the UI, and save it only when complete.

use crate::meetings::claude::{build_request, cost_usd, stream_message, ClaudeError, Completed, KeyStore};
use crate::meetings::session::NOT_TRANSCRIBED;
use crate::meetings::{Length, MeetingRow, NoteRow, SegmentRow};
use crate::store::Store;
use serde::Serialize;
use serde_json::Value;

pub const SYSTEM_PROMPT: &str = "\
You turn a work meeting into clear notes for the person who recorded it.

You get the meeting's details, the recorder's own rough notes, and a transcript. Each transcript line is \
\"[time] Name: what they said\". Lines named \"Speaker\" come from someone in the call whose name isn't known.

How to write:
- British English. Plain, simple wording; short sentences. Detailed enough that someone who missed the \
meeting understands what happened, but no padding.
- Use only what the transcript and the recorder's notes say. Never invent facts, numbers, dates, owners or \
decisions. If something is unclear in the transcript, say so briefly rather than guessing.
- Say who said or agreed what, using the names in the transcript. For a line from \"Speaker\", write \
\"someone\" (or \"another participant\") instead of guessing a name.
- The recorder's own notes show what mattered to them: make sure those points are covered.
- The transcript is machine-made and may mishear words; correct obvious mis-hearings only when the meaning \
is certain from context.
- Output only the notes in Markdown, starting with the first heading. No preamble, no closing remarks. \
If a section has nothing in it, write \"None.\" under its heading.";

/// The length instruction (second, uncached block).
pub fn instruction(len: Length) -> &'static str {
    match len {
        Length::Short => "\
Write SHORT notes (about half a page) with exactly these sections:
## Summary
3–5 sentences on what the meeting was about and what came out of it.
## Key points
At most 8 bullets.
## Action items
Bullets as \"- **Owner** — task — due date if one was said\".",
        Length::Medium => "\
Write MEDIUM notes (about one page) with exactly these sections:
## Summary
3–5 sentences.
## Topics discussed
One ### heading per topic, in the order discussed, with bullets under each.
## Decisions
Bullets.
## Action items
Bullets as \"- **Owner** — task — due date if one was said\" (Owner — task — due).
## Open questions
Bullets for anything left unresolved.",
        Length::Long => "\
Write LONG, detailed notes (several pages) with exactly these sections:
## Summary
A short paragraph.
## Topics discussed
One ### heading per topic, in the order discussed, with bullets under each.
## Decisions
Bullets, with who made or agreed each decision.
## Action items
Bullets as \"- **Owner** — task — due date if one was said\" (Owner — task — due).
## Open questions
Bullets for anything left unresolved.
## Walkthrough
Topic by topic, in order: who said what, the reasoning given, and every number, date and figure mentioned.",
    }
}

fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn when(m: &MeetingRow) -> String {
    let parse = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&chrono::Local));
    let Some(start) = parse(&m.started_at_utc) else { return String::new() };
    let mins = (m.duration_ms / 60_000).max(1);
    match m.ended_at_utc.as_deref().and_then(parse) {
        Some(end) => format!("{}–{} ({mins} min)", start.format("%-d %b %Y, %H:%M"), end.format("%H:%M")),
        None => format!("{} ({mins} min)", start.format("%-d %b %Y, %H:%M")),
    }
}

/// The cached block: meeting facts, the user's notes, the transcript (muted lines left out).
pub fn context_block(m: &MeetingRow, segs: &[SegmentRow], my_notes_md: &str, me_name: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("Meeting: {}\n", m.title));
    out.push_str(&format!("Date: {}\n", when(m)));
    if !m.participants.is_empty() {
        out.push_str(&format!("Participants: {}\n", m.participants.join(", ")));
    }
    match m.teams_self_name.as_deref().filter(|t| !t.eq_ignore_ascii_case(me_name)) {
        Some(t) => out.push_str(&format!("Me: {me_name} (also appears in Teams as \"{t}\")\n")),
        None => out.push_str(&format!("Me: {me_name}\n")),
    }
    out.push_str("\nMy notes:\n");
    let notes = my_notes_md.trim();
    out.push_str(if notes.is_empty() { "(none)" } else { notes });
    out.push_str("\n\nTranscript:\n");
    for s in segs.iter().filter(|s| !s.muted) {
        let who = s.speaker.as_deref().filter(|n| !n.trim().is_empty()).unwrap_or("Speaker");
        out.push_str(&format!("[{}] {}: {}\n", clock(s.start_ms), who, s.text.trim()));
    }
    out
}

// ---- TipTap JSON → Markdown ----

fn inline(nodes: &Value) -> String {
    let mut out = String::new();
    for n in nodes.as_array().into_iter().flatten() {
        match n["type"].as_str() {
            Some("text") => {
                let mut t = n["text"].as_str().unwrap_or("").to_string();
                for m in n["marks"].as_array().into_iter().flatten() {
                    t = match m["type"].as_str() {
                        Some("bold") => format!("**{t}**"),
                        Some("italic") => format!("*{t}*"),
                        Some("code") => format!("`{t}`"),
                        Some("strike") => format!("~~{t}~~"),
                        _ => t,
                    };
                }
                out.push_str(&t);
            }
            Some("hardBreak") => out.push('\n'),
            _ => out.push_str(&inline(&n["content"])),
        }
    }
    out
}

fn list(node: &Value, depth: usize) -> String {
    let kind = node["type"].as_str().unwrap_or("");
    let mut lines = Vec::new();
    for (i, item) in node["content"].as_array().into_iter().flatten().enumerate() {
        let marker = match kind {
            "orderedList" => format!("{}.", i + 1),
            "taskList" => format!("- [{}]", if item["attrs"]["checked"].as_bool().unwrap_or(false) { "x" } else { " " }),
            _ => "-".to_string(),
        };
        let mut text = String::new();
        let mut nested = Vec::new();
        for c in item["content"].as_array().into_iter().flatten() {
            match c["type"].as_str() {
                Some("bulletList" | "orderedList" | "taskList") => nested.push(list(c, depth + 1)),
                _ => {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(&inline(&c["content"]));
                }
            }
        }
        lines.push(format!("{}{} {}", "  ".repeat(depth), marker, text));
        lines.extend(nested);
    }
    lines.join("\n")
}

/// The My-notes editor saves TipTap JSON; Claude reads Markdown. Non-JSON text passes through.
pub fn tiptap_to_markdown(doc: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(doc) else { return doc.trim().to_string() };
    let mut blocks = Vec::new();
    for b in v["content"].as_array().into_iter().flatten() {
        let s = match b["type"].as_str() {
            Some("heading") => {
                let level = b["attrs"]["level"].as_u64().unwrap_or(2).clamp(1, 6) as usize;
                format!("{} {}", "#".repeat(level), inline(&b["content"]))
            }
            Some("bulletList" | "orderedList" | "taskList") => list(b, 0),
            Some("blockquote") => format!("> {}", inline(&b["content"])),
            Some("horizontalRule") => "---".to_string(),
            _ => inline(&b["content"]),
        };
        if !s.trim().is_empty() {
            blocks.push(s);
        }
    }
    blocks.join("\n\n")
}

// ---- generation ----

/// What the UI hears while a note is written (`meeting://note`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum NoteEvent {
    /// A run began (sent before Claude's first token, so the page shows "Writing…" at once).
    #[serde(rename_all = "camelCase")]
    Started { meeting_id: String, length: Length },
    #[serde(rename_all = "camelCase")]
    Delta { meeting_id: String, length: Length, text: String },
    Done { note: NoteRow },
    #[serde(rename_all = "camelCase")]
    Error { meeting_id: String, length: Length, kind: String, message: String },
}

/// Sends the request (Claude in the app, a fake in tests).
pub trait NoteWriter {
    fn write(&self, body: &Value, on_delta: &mut dyn FnMut(&str)) -> Result<Completed, ClaudeError>;
}

/// The real writer: the key from the OS credential store, streamed over HTTPS.
pub struct ClaudeWriter;

impl NoteWriter for ClaudeWriter {
    fn write(&self, body: &Value, on_delta: &mut dyn FnMut(&str)) -> Result<Completed, ClaudeError> {
        let key = KeyStore::system().get().ok_or(ClaudeError::NoKey)?;
        stream_message(&key, body, on_delta)
    }
}

/// Write one note. Streams deltas through `emit`; saves (and marks fresh) only on success, so a failed
/// or interrupted run leaves any earlier version of that length in place.
pub fn generate(
    store: &Store,
    writer: &dyn NoteWriter,
    model: &str,
    me_name: &str,
    id: &str,
    len: Length,
    emit: &mut dyn FnMut(NoteEvent),
) -> Result<NoteRow, ClaudeError> {
    let fail = |e: ClaudeError, emit: &mut dyn FnMut(NoteEvent)| {
        emit(NoteEvent::Error { meeting_id: id.to_string(), length: len, kind: e.kind().into(), message: e.to_string() });
        Err(e)
    };
    let started = chrono::Utc::now();
    let m = match store.meeting_get(id) {
        Ok(Some(m)) => m,
        _ => return fail(ClaudeError::Api("That meeting no longer exists.".into()), emit),
    };
    let segs = store.segments(id).unwrap_or_default();
    let notes_md = tiptap_to_markdown(&m.my_notes);
    let said = segs.iter().any(|s| !s.muted && s.text != NOT_TRANSCRIBED);
    if !said && notes_md.trim().is_empty() {
        return fail(ClaudeError::Api("There's nothing to summarise yet.".into()), emit);
    }
    let body = build_request(model, SYSTEM_PROMPT, &context_block(&m, &segs, &notes_md, me_name), instruction(len));
    tracing::info!(meeting = %id, length = len.as_db(), lines = segs.len(), "writing enhanced notes");
    let done = {
        let mut on_delta = |t: &str| emit(NoteEvent::Delta { meeting_id: id.to_string(), length: len, text: t.to_string() });
        writer.write(&body, &mut on_delta)
    };
    let done = match done {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(meeting = %id, kind = e.kind(), "enhanced notes failed");
            return fail(e, emit);
        }
    };
    let note = NoteRow {
        meeting_id: id.to_string(),
        length: len,
        markdown: done.text.trim().to_string(),
        model: model.to_string(),
        input_tokens: done.usage.input,
        output_tokens: done.usage.output,
        cache_read_tokens: done.usage.cache_read,
        cache_write_tokens: done.usage.cache_write,
        cost_usd: cost_usd(model, &done.usage),
        created_at_utc: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        stale: false,
    };
    match store.note_save_if_current(&note, started) {
        Ok(true) => {}
        Ok(false) => return fail(ClaudeError::Api("That meeting was deleted.".into()), emit),
        Err(e) => return fail(ClaudeError::Api(format!("couldn't save the notes ({e})")), emit),
    }
    // Saved stale if the notes or names changed while Claude was writing.
    let note = store.notes(id).ok().and_then(|ns| ns.into_iter().find(|n| n.length == len)).unwrap_or(note);
    tracing::info!(meeting = %id, length = len.as_db(), out_tokens = note.output_tokens, "enhanced notes saved");
    emit(NoteEvent::Done { note: note.clone() });
    Ok(note)
}

#[cfg(test)]
#[path = "enhance_tests.rs"]
mod tests;
