//! Claude client for enhanced meeting notes (meetings spec §5.4, plan Task 9): the Messages API
//! request, a streaming (SSE) parser and the retry loop. The key store, error mapping, usage/cost
//! and the HTTPS POST are shared with AI clean-up in [`crate::claude_api`] (re-exported here).
//!
//! Raw HTTPS (there is no official Rust SDK). The key is never logged, serialised or sent to the UI.

use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

pub use crate::claude_api::{
    cost_usd, error_from_kind, error_from_status, looks_like_key, ClaudeError, KeyStore, Secret, Usage, API_URL, API_VERSION,
};

/// Server-side refusal fallback (`fallbacks: "default"`).
pub const BETA: &str = "server-side-fallback-2026-07-01";
pub const MAX_TOKENS: u32 = 16_000;
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Model used only to check a pasted key.
const TEST_MODEL: &str = "claude-sonnet-5-5";
/// Retry waits for 429 / 529 / network errors, used only before any text has streamed.
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(6), Duration::from_secs(15)];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A long note can stream for minutes.
const STREAM_TIMEOUT: Duration = Duration::from_secs(600);

// ---- request ----

/// The Messages API body: fixed system prompt, then one user turn whose first block (meeting facts,
/// notes, transcript) is cached so switching length reuses it, and whose second block is the length
/// instruction.
pub fn build_request(model: &str, system: &str, context_block: &str, instruction: &str) -> Value {
    json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "output_config": { "effort": "medium" },
        "fallbacks": "default",
        "system": [{ "type": "text", "text": system }],
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": context_block, "cache_control": { "type": "ephemeral" } },
                { "type": "text", "text": instruction }
            ]
        }]
    })
}

// ---- streaming ----

#[derive(Debug, Clone, PartialEq)]
pub enum SseEvent {
    TextDelta(String),
    Usage(Usage),
    Stop(String),
    Error { kind: String, message: String },
    /// The stream's last event: only a stream that reaches it is complete.
    MessageStop,
    Ignored,
}

/// Bytes of a partly received event, kept between chunks.
#[derive(Default)]
pub struct SseState {
    buf: String,
}

fn event_of(data: &str) -> Vec<SseEvent> {
    let Ok(v) = serde_json::from_str::<Value>(data) else { return vec![SseEvent::Ignored] };
    match v.get("type").and_then(Value::as_str).unwrap_or("") {
        "message_start" => vec![SseEvent::Usage(Usage::from_json(&v["message"]["usage"]))],
        "content_block_delta" if v["delta"]["type"] == "text_delta" => {
            vec![SseEvent::TextDelta(v["delta"]["text"].as_str().unwrap_or("").to_string())]
        }
        "message_delta" => {
            let mut out = vec![SseEvent::Usage(Usage::from_json(&v["usage"]))];
            if let Some(r) = v["delta"]["stop_reason"].as_str() {
                out.push(SseEvent::Stop(r.to_string()));
            }
            out
        }
        "message_stop" => vec![SseEvent::MessageStop],
        "error" => vec![SseEvent::Error {
            kind: v["error"]["type"].as_str().unwrap_or("error").to_string(),
            message: v["error"]["message"].as_str().unwrap_or("").to_string(),
        }],
        _ => vec![SseEvent::Ignored],
    }
}

/// Feed raw stream text; returns the events completed by it (events are separated by a blank line).
pub fn parse_sse(chunk: &str, st: &mut SseState) -> Vec<SseEvent> {
    st.buf.extend(chunk.chars().filter(|&c| c != '\r'));
    let mut out = Vec::new();
    while let Some(end) = st.buf.find("\n\n") {
        let block: String = st.buf.drain(..end + 2).collect();
        let data: Vec<&str> = block
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.strip_prefix(' ').unwrap_or(d))
            .collect();
        if !data.is_empty() {
            out.extend(event_of(&data.join("\n")));
        }
    }
    out
}

// ---- errors ----

/// A stop reason that means "no usable note".
pub fn stop_error(stop_reason: &str) -> Option<ClaudeError> {
    (stop_reason == "refusal").then_some(ClaudeError::Refusal)
}

/// Why a finished stream can't be kept: it never reached `message_stop`, had no stop reason, or was
/// refused. `None` means the note is complete.
pub fn completion_error(stop_reason: &str, saw_message_stop: bool) -> Option<ClaudeError> {
    if !saw_message_stop || stop_reason.is_empty() {
        return Some(ClaudeError::Network("the stream ended early".into()));
    }
    stop_error(stop_reason)
}

// ---- labels ----

pub fn model_label(model: &str) -> &'static str {
    match model {
        "claude-sonnet-5-5" => "Claude Sonnet 5.5",
        _ => "Claude Opus 5.5",
    }
}

// ---- HTTP ----

#[derive(Debug, Clone, PartialEq)]
pub struct Completed {
    pub text: String,
    pub usage: Usage,
    pub stop_reason: String,
}

fn client() -> Result<reqwest::blocking::Client, ClaudeError> {
    crate::claude_api::client(CONNECT_TIMEOUT, STREAM_TIMEOUT)
}

fn post(client: &reqwest::blocking::Client, key: &str, body: &Value) -> Result<reqwest::blocking::Response, ClaudeError> {
    crate::claude_api::post(client, API_URL, key, Some(BETA), None, body)
}

/// One streamed attempt. `on_delta` gets each piece of text as it arrives.
fn stream_once(
    client: &reqwest::blocking::Client,
    key: &str,
    body: &Value,
    on_delta: &mut dyn FnMut(&str),
    got_text: &mut bool,
) -> Result<Completed, ClaudeError> {
    let mut resp = post(client, key, body)?;
    let mut st = SseState::default();
    let mut done = Completed { text: String::new(), usage: Usage::default(), stop_reason: String::new() };
    let mut raw = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut saw_stop = false;
    loop {
        if saw_stop {
            break; // the final event arrived: don't wait on the connection closing
        }
        let n = resp.read(&mut raw).map_err(|_| ClaudeError::Network("the connection dropped".into()))?;
        if n == 0 {
            break;
        }
        pending.extend_from_slice(&raw[..n]);
        // Only hand over whole UTF-8 characters.
        let valid = match std::str::from_utf8(&pending) {
            Ok(s) => s.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
        pending.drain(..valid);
        for ev in parse_sse(&text, &mut st) {
            match ev {
                SseEvent::TextDelta(t) => {
                    *got_text = true;
                    on_delta(&t);
                    done.text.push_str(&t);
                }
                SseEvent::Usage(u) => done.usage.merge(&u),
                SseEvent::Stop(r) => done.stop_reason = r,
                SseEvent::Error { kind, message } => return Err(error_from_kind(&kind, &message)),
                SseEvent::MessageStop => saw_stop = true,
                SseEvent::Ignored => {}
            }
        }
    }
    if let Some(e) = completion_error(&done.stop_reason, saw_stop) {
        return Err(e);
    }
    Ok(done)
}

/// Stream a message. Retries 429 / 529 / network errors with backoff, but only before any text has
/// arrived; after that the error is returned and the caller discards the partial text.
pub fn stream_message(key: &Secret, body: &Value, on_delta: &mut dyn FnMut(&str)) -> Result<Completed, ClaudeError> {
    let client = client()?;
    let mut attempt = 0;
    loop {
        let mut got_text = false;
        match stream_once(&client, key.expose(), body, on_delta, &mut got_text) {
            Ok(c) => return Ok(c),
            Err(e) if e.retryable() && !got_text && attempt < BACKOFF.len() => {
                tracing::info!(kind = e.kind(), attempt, "Claude request failed; retrying");
                std::thread::sleep(BACKOFF[attempt]);
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Spend a tiny request to check that `key` works.
pub fn test_key(key: &str) -> Result<(), ClaudeError> {
    let client = client()?;
    let body = json!({
        "model": TEST_MODEL,
        "max_tokens": 16,
        "messages": [{ "role": "user", "content": "Reply OK" }]
    });
    post(&client, key.trim(), &body).map(|_| ())
}

#[cfg(test)]
#[path = "claude_tests.rs"]
mod tests;
