//! Optional AI clean-up (spec §10, off by default) through Claude, styled for the app the text is
//! going into (brief 2026-11-26-claude-ai-cleanup). The tidied text goes to Claude with the
//! Meeting-notes API key, wrapped in delimiters, with a fixed prompt for the app's [`AppCategory`]
//! that never lets the model answer or obey it. `pipeline::run` then checks the reply for that mode
//! and pastes the tidied text instead if it fails, or if Claude is slower than
//! `AI_CLEANUP_TIMEOUT_MS`, unreachable, or there is no key.
//!
//! One non-streaming `POST /v1/messages` (API version 2026-10-21): no `temperature`, no assistant
//! prefill, `output_config.effort: "low"` for speed, worked examples as earlier turns, and the fixed
//! prompt + examples marked for caching when they are long enough to be cached.
//!
//! Privacy: only the text is sent (never audio, never the window title). Logs carry timings, sizes,
//! the category and outcomes, never the text.

pub mod category;
pub mod prompts;

pub use category::{category, AppCategory};

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::claude_api::{self, ClaudeError, KeyStore, Usage, API_URL};
use crate::constants::{
    AI_CLEANUP_CACHE_MIN_TOKENS, AI_CLEANUP_MAX_TOKENS_CAP, AI_CLEANUP_MIN_TOKENS, AI_CLEANUP_TIMEOUT_MAX_MS,
    AI_CLEANUP_TIMEOUT_MS, AI_CLEANUP_TIMEOUT_PER_WORD_MS, AI_CLEANUP_TOKENS_PER_WORD,
};
use crate::settings::CodingStyle;

const OPEN: &str = "<transcript>";
const CLOSE: &str = "</transcript>";

/// `text` with every `tag` (ASCII, any case) passed to `f`, the rest copied.
fn map_tag(text: &str, tag: &str, f: impl Fn(&str) -> String) -> String {
    let lower = text.to_ascii_lowercase(); // same byte offsets as `text`
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(i) = lower[at..].find(tag) {
        let start = at + i;
        out.push_str(&text[at..start]);
        out.push_str(&f(&text[start..start + tag.len()]));
        at = start + tag.len();
    }
    out.push_str(&text[at..]);
    out
}

/// The user message: the text inside the delimiters (a delimiter typed in the text, in any case,
/// is defused).
pub fn user_message(text: &str) -> String {
    let defuse = |t: &str| format!("{} >", &t[..t.len() - 1]);
    let safe = map_tag(&map_tag(text, OPEN, defuse), CLOSE, defuse);
    format!("{OPEN}\n{safe}\n{CLOSE}")
}

/// Longest reply allowed, in tokens (thinking included).
pub fn max_tokens(text: &str) -> usize {
    (AI_CLEANUP_MIN_TOKENS + text.split_whitespace().count() * AI_CLEANUP_TOKENS_PER_WORD).min(AI_CLEANUP_MAX_TOKENS_CAP)
}

/// How long the clean-up of `text` may take: a network round trip plus time to write a long reply.
pub fn timeout(text: &str) -> Duration {
    let words = text.split_whitespace().count() as u64;
    Duration::from_millis((AI_CLEANUP_TIMEOUT_MS + words * AI_CLEANUP_TIMEOUT_PER_WORD_MS).min(AI_CLEANUP_TIMEOUT_MAX_MS))
}

/// Rough token count of fixed prompt text: about 3 characters a token on the 5.5 models'
/// tokenizer, measured against the API's `usage` (the General prefix counted ~540 tokens). Only
/// the cache decision uses it; a marker on a prefix that turns out too short is simply ignored.
fn estimate_tokens(s: &str) -> usize {
    s.chars().count() / 3
}

/// The Messages API body for cleaning `text` going into `target`.
pub fn request_body(model: &str, target: AppCategory, coding_style: CodingStyle, text: &str) -> Value {
    let (system, examples) = prompts::for_target(target, coding_style);
    let prefix_tokens = estimate_tokens(&system)
        + examples.iter().map(|(h, c)| estimate_tokens(&user_message(h)) + estimate_tokens(c)).sum::<usize>();
    let cache = prefix_tokens >= AI_CLEANUP_CACHE_MIN_TOKENS;
    let mut messages = Vec::with_capacity(examples.len() * 2 + 1);
    for (i, (heard, cleaned)) in examples.iter().enumerate() {
        messages.push(json!({ "role": "user", "content": user_message(heard) }));
        let mut block = json!({ "type": "text", "text": cleaned });
        if cache && i + 1 == examples.len() {
            // The last fixed turn: system prompt + all examples are the cached prefix.
            block["cache_control"] = json!({ "type": "ephemeral" });
        }
        messages.push(json!({ "role": "assistant", "content": [block] }));
    }
    messages.push(json!({ "role": "user", "content": user_message(text) }));
    json!({
        "model": model,
        "max_tokens": max_tokens(text),
        "output_config": { "effort": "low" },
        "system": [{ "type": "text", "text": system }],
        "messages": messages,
    })
}

/// The model's text, without delimiters, a code fence or wrapping quotes it may have added (when
/// the original had none). None if empty.
pub fn extract_reply(reply: &str, original: &str) -> Option<String> {
    let mut s = reply.trim();
    if s.len() >= OPEN.len() && s.is_char_boundary(OPEN.len()) && s[..OPEN.len()].eq_ignore_ascii_case(OPEN) {
        s = &s[OPEN.len()..];
    }
    let cut = s.len().wrapping_sub(CLOSE.len());
    if s.len() >= CLOSE.len() && s.is_char_boundary(cut) && s[cut..].eq_ignore_ascii_case(CLOSE) {
        s = &s[..cut];
    }
    s = s.trim();
    let orig = original.trim();
    if s.starts_with("```") && s.ends_with("```") && s.len() > 6 && !orig.starts_with("```") {
        let inner = &s[3..s.len() - 3];
        // Drop a language tag on the opening line ("```text").
        s = match inner.split_once('\n') {
            Some((first, rest)) if !first.trim().contains(' ') => rest,
            _ => inner,
        }
        .trim();
    }
    let quoted = |s: &str, a: char, b: char| s.len() >= 2 && s.starts_with(a) && s.ends_with(b);
    for (a, b) in [('"', '"'), ('\u{201C}', '\u{201D}'), ('\'', '\'')] {
        if quoted(s, a, b) && !quoted(orig, a, b) {
            s = s[a.len_utf8()..s.len() - b.len_utf8()].trim();
        }
    }
    (!s.is_empty()).then(|| s.to_string())
}

/// The text and usage of a Messages API response. A reply that did not finish (`max_tokens`,
/// `refusal`…) has no text.
pub fn parse_response(body: &Value, original: &str) -> (Option<String>, Usage, String) {
    let usage = Usage::from_json(&body["usage"]);
    let stop = body["stop_reason"].as_str().unwrap_or("").to_string();
    let text: String = body["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let finished = matches!(stop.as_str(), "end_turn" | "stop_sequence");
    (if finished { extract_reply(&text, original) } else { None }, usage, stop)
}

/// One clean-up request's result.
#[derive(Debug, Clone, PartialEq)]
pub struct Cleaned {
    /// Claude's version of the text (None: empty, refused or cut off).
    pub text: Option<String>,
    /// What the request used (it is billed even when the text is not usable).
    pub usage: Usage,
    pub ms: u64,
}

/// The clean-up request through `client` to `url` (the Messages API, or a fake in tests).
#[allow(clippy::too_many_arguments)]
pub fn clean_with(
    client: &reqwest::blocking::Client,
    url: &str,
    key: &str,
    model: &str,
    target: AppCategory,
    coding_style: CodingStyle,
    text: &str,
) -> Result<Cleaned, ClaudeError> {
    let t = Instant::now();
    let body = request_body(model, target, coding_style, text);
    let resp = claude_api::post(client, url, key, None, Some(timeout(text)), &body)?;
    let json: Value = resp
        .json()
        .map_err(|e| ClaudeError::Network(if e.is_timeout() { "timed out".into() } else { "bad response".into() }))?;
    let (reply, usage, stop) = parse_response(&json, text);
    let ms = t.elapsed().as_millis() as u64;
    if reply.is_none() {
        tracing::info!(ms, stop = %stop, category = target.as_str(), "AI clean-up gave no usable text");
    }
    Ok(Cleaned { text: reply, usage, ms })
}

/// One client for every clean-up, so its pooled connection to the API is reused (and can be opened
/// ahead of time by [`preconnect`]). Each request is bounded by [`timeout`].
static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| {
    claude_api::client(Duration::from_millis(AI_CLEANUP_TIMEOUT_MS), Duration::from_millis(AI_CLEANUP_TIMEOUT_MAX_MS)).ok()
});

/// The clean-up step: Claude's version of `text` going into `target`, with the key saved under
/// Meeting notes. Errors (no key, slow, offline, refused) mean the plain text is pasted.
pub fn clean(model: &str, target: AppCategory, coding_style: CodingStyle, text: &str) -> Result<Cleaned, ClaudeError> {
    let key = KeyStore::system().get().ok_or(ClaudeError::NoKey)?;
    let client = CLIENT.as_ref().ok_or_else(|| ClaudeError::Network("no HTTP client".into()))?;
    let t = Instant::now();
    let r = clean_with(client, API_URL, key.expose(), model, target, coding_style, text);
    match &r {
        Ok(c) => tracing::info!(
            ms = c.ms,
            category = target.as_str(),
            chars_in = text.chars().count(),
            chars_out = c.text.as_ref().map(|s| s.chars().count()),
            input_tokens = c.usage.input,
            cache_read = c.usage.cache_read,
            cache_write = c.usage.cache_write,
            output_tokens = c.usage.output,
            "AI clean-up reply"
        ),
        Err(e) => tracing::info!(kind = e.kind(), ms = t.elapsed().as_millis() as u64, category = target.as_str(), "AI clean-up skipped"),
    }
    r
}

/// Opens the HTTPS connection to the API while the user is still speaking (when a recording starts),
/// so the clean-up request skips the TLS handshake. Sends no key and no text: a bare `HEAD`, whose
/// answer is ignored. Only when a key is saved.
pub fn preconnect() {
    if KeyStore::system().get().is_none() {
        return;
    }
    let Some(client) = CLIENT.as_ref() else { return };
    let t = Instant::now();
    match client.head(API_URL).send() {
        Ok(r) => tracing::debug!(status = r.status().as_u16(), ms = t.elapsed().as_millis() as u64, "AI clean-up connection opened"),
        Err(e) => tracing::info!(timeout = e.is_timeout(), "AI clean-up connection could not be opened"),
    }
}

#[cfg(test)]
mod tests;
