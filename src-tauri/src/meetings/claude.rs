//! Claude client for enhanced meeting notes (meetings spec §5.4, plan Task 9): the API key in the OS
//! credential store, the Messages API request, a streaming (SSE) parser, error mapping and cost.
//!
//! Raw HTTPS (there is no official Rust SDK). The key is never logged, serialised or sent to the UI.

use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2026-10-21";
/// Server-side refusal fallback (`fallbacks: "default"`).
pub const BETA: &str = "server-side-fallback-2026-07-01";
pub const MAX_TOKENS: u32 = 16_000;
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Model used only to check a pasted key.
const TEST_MODEL: &str = "claude-sonnet-5-5";
const KEY_SERVICE: &str = "Wren";
const KEY_ACCOUNT: &str = "anthropic-api-key";
/// Retry waits for 429 / 529 / network errors, used only before any text has streamed.
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(6), Duration::from_secs(15)];
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A long note can stream for minutes.
const STREAM_TIMEOUT: Duration = Duration::from_secs(600);

// ---- the key ----

/// An API key. `Debug` never shows it; it has no `Serialize`.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: String) -> Self {
        Self(s)
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Secret(<{} chars>)", self.0.len())
    }
}

/// Plausible Anthropic key shape (checked before a test call is spent on it).
pub fn looks_like_key(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("sk-ant-") && t.len() >= 40 && !t.chars().any(char::is_whitespace)
}

enum Backing {
    Memory(parking_lot::Mutex<Option<String>>),
    System,
}

/// Where the key lives: Windows Credential Manager / macOS Keychain (service "Wren").
/// `WREN_KEYSTORE_MEMORY=1` makes [`KeyStore::system`] in-memory (tests, CI).
pub struct KeyStore(Backing);

static MEMORY_SYSTEM: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

impl KeyStore {
    pub fn system() -> Self {
        KeyStore(Backing::System)
    }

    pub fn memory() -> Self {
        KeyStore(Backing::Memory(parking_lot::Mutex::new(None)))
    }

    fn memory_seam() -> bool {
        std::env::var("WREN_KEYSTORE_MEMORY").is_ok_and(|v| v == "1")
    }

    pub fn get(&self) -> Option<Secret> {
        match &self.0 {
            Backing::Memory(m) => m.lock().clone().map(Secret),
            Backing::System if Self::memory_seam() => MEMORY_SYSTEM.lock().clone().map(Secret),
            Backing::System => keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT)
                .ok()?
                .get_password()
                .ok()
                .filter(|k| !k.is_empty())
                .map(Secret),
        }
    }

    pub fn set(&self, key: &str) -> Result<(), String> {
        let key = key.trim().to_string();
        match &self.0 {
            Backing::Memory(m) => *m.lock() = Some(key),
            Backing::System if Self::memory_seam() => *MEMORY_SYSTEM.lock() = Some(key),
            Backing::System => keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT)
                .and_then(|e| e.set_password(&key))
                .map_err(|_| "Couldn't save the key in the system's secure storage.".to_string())?,
        }
        Ok(())
    }

    pub fn remove(&self) -> Result<(), String> {
        match &self.0 {
            Backing::Memory(m) => *m.lock() = None,
            Backing::System if Self::memory_seam() => *MEMORY_SYSTEM.lock() = None,
            Backing::System => match keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).and_then(|e| e.delete_credential()) {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(_) => return Err("Couldn't remove the key from the system's secure storage.".into()),
            },
        }
        Ok(())
    }
}

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl Usage {
    /// Fold a later usage report in (message_delta carries cumulative output).
    pub fn merge(&mut self, u: &Usage) {
        for (mine, theirs) in [
            (&mut self.input, u.input),
            (&mut self.output, u.output),
            (&mut self.cache_read, u.cache_read),
            (&mut self.cache_write, u.cache_write),
        ] {
            if theirs > 0 {
                *mine = theirs;
            }
        }
    }

    fn from_json(v: &Value) -> Usage {
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        }
    }
}

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

#[derive(Debug, Clone, PartialEq)]
pub enum ClaudeError {
    NoKey,
    Unauthorized,
    RateLimited,
    Overloaded,
    Network(String),
    Refusal,
    Api(String),
}

impl ClaudeError {
    pub fn retryable(&self) -> bool {
        matches!(self, ClaudeError::RateLimited | ClaudeError::Overloaded | ClaudeError::Network(_))
    }

    /// Short machine kind for the UI (`note-error` events).
    pub fn kind(&self) -> &'static str {
        match self {
            ClaudeError::NoKey => "no_key",
            ClaudeError::Unauthorized => "unauthorized",
            ClaudeError::RateLimited => "rate_limited",
            ClaudeError::Overloaded => "overloaded",
            ClaudeError::Network(_) => "network",
            ClaudeError::Refusal => "refusal",
            ClaudeError::Api(_) => "api",
        }
    }
}

impl std::fmt::Display for ClaudeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaudeError::NoKey => write!(f, "Add your Claude API key in Settings."),
            ClaudeError::Unauthorized => write!(f, "Claude rejected the API key."),
            ClaudeError::RateLimited => write!(f, "Claude is rate-limiting requests right now."),
            ClaudeError::Overloaded => write!(f, "Claude is overloaded right now."),
            ClaudeError::Network(m) => write!(f, "Couldn't reach Claude ({m})."),
            ClaudeError::Refusal => write!(f, "Claude declined to summarise this meeting."),
            ClaudeError::Api(m) => write!(f, "Claude returned an error: {m}"),
        }
    }
}

pub fn error_from_kind(kind: &str, message: &str) -> ClaudeError {
    match kind {
        "authentication_error" | "permission_error" => ClaudeError::Unauthorized,
        "rate_limit_error" => ClaudeError::RateLimited,
        "overloaded_error" => ClaudeError::Overloaded,
        "api_error" => ClaudeError::Network(message.to_string()),
        _ => ClaudeError::Api(message.to_string()),
    }
}

pub fn error_from_status(status: u16, body: &str) -> ClaudeError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("HTTP {status}"));
    match status {
        401 | 403 => ClaudeError::Unauthorized,
        429 => ClaudeError::RateLimited,
        529 => ClaudeError::Overloaded,
        500..=599 => ClaudeError::Network(format!("server error {status}")),
        _ => ClaudeError::Api(message),
    }
}

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

// ---- cost ----

/// List prices, $/MTok: (input, output, cache read). Cache writes (5 min) cost 1.25 × input.
fn prices(model: &str) -> (f64, f64, f64) {
    if model.starts_with("claude-sonnet") { (2.0, 10.0, 0.10) } else { (4.0, 20.0, 0.20) }
}

pub fn cost_usd(model: &str, u: &Usage) -> f64 {
    let (i, o, r) = prices(model);
    (u.input as f64 * i + u.output as f64 * o + u.cache_read as f64 * r + u.cache_write as f64 * i * 1.25) / 1_000_000.0
}

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
    reqwest::blocking::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(STREAM_TIMEOUT)
        // The key travels in a custom header that redirects would carry along: never follow one.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ClaudeError::Network(e.to_string()))
}

fn post(client: &reqwest::blocking::Client, key: &str, body: &Value) -> Result<reqwest::blocking::Response, ClaudeError> {
    let resp = client
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", BETA)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .map_err(|e| ClaudeError::Network(if e.is_timeout() { "timed out".into() } else { "no connection".into() }))?;
    let status = resp.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(resp);
    }
    let text = resp.text().unwrap_or_default();
    Err(error_from_status(status, &text))
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
