//! What enhanced meeting notes (`meetings::claude`) and AI clean-up (`ai_cleanup`) share about
//! talking to Claude: the API key in the OS credential store, error mapping, token usage and its
//! list-price cost, and the HTTPS POST itself.
//!
//! Raw HTTPS (there is no official Rust SDK). The key is never logged, serialised or sent to the UI.

use serde_json::Value;
use std::time::Duration;

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2026-10-21";
const KEY_SERVICE: &str = "Wren";
const KEY_ACCOUNT: &str = "anthropic-api-key";

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

/// Where the key lives: Windows Credential Manager (service "Wren"); on macOS a private file in
/// Wren's folder ([`crate::claude_key_file`] says why), moved there from the Keychain on first read.
/// Read once per launch, so the OS store is never asked on every dictation.
/// `WREN_KEYSTORE_MEMORY=1` makes [`KeyStore::system`] in-memory (tests, CI).
pub struct KeyStore(Backing);

static MEMORY_SYSTEM: parking_lot::Mutex<Option<String>> = parking_lot::Mutex::new(None);

/// In-memory key cache for one launch (`Some(None)` = known to be absent).
/// Held while loading, so concurrent callers can't both trigger a Keychain prompt.
struct KeyCache(parking_lot::Mutex<Option<Option<String>>>);

impl KeyCache {
    const fn new() -> Self {
        Self(parking_lot::Mutex::new(None))
    }

    fn get_or_load(&self, load: impl FnOnce() -> (Option<String>, bool)) -> Option<String> {
        let mut cache = self.0.lock();
        if let Some(known) = cache.as_ref() {
            return known.clone();
        }
        let (key, settled) = load();
        if settled {
            *cache = Some(key.clone());
        }
        key
    }

    fn set(&self, key: String, save: impl FnOnce(&str) -> Result<(), String>) -> Result<(), String> {
        let mut cache = self.0.lock();
        save(&key)?;
        *cache = Some((!key.is_empty()).then_some(key));
        Ok(())
    }

    fn remove(&self, delete: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        let mut cache = self.0.lock();
        delete()?;
        *cache = Some(None);
        Ok(())
    }
}

static SYSTEM_CACHE: KeyCache = KeyCache::new();

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
            Backing::System => SYSTEM_CACHE.get_or_load(system_store::load).map(Secret),
        }
    }

    pub fn set(&self, key: &str) -> Result<(), String> {
        let key = key.trim().to_string();
        match &self.0 {
            Backing::Memory(m) => *m.lock() = Some(key),
            Backing::System if Self::memory_seam() => *MEMORY_SYSTEM.lock() = Some(key),
            Backing::System => SYSTEM_CACHE.set(key, system_store::save)?,
        }
        Ok(())
    }

    pub fn remove(&self) -> Result<(), String> {
        match &self.0 {
            Backing::Memory(m) => *m.lock() = None,
            Backing::System if Self::memory_seam() => *MEMORY_SYSTEM.lock() = None,
            Backing::System => SYSTEM_CACHE.remove(system_store::delete)?,
        }
        Ok(())
    }
}

/// The OS credential store (service "Wren", account "anthropic-api-key").
mod os_store {
    use super::{KEY_ACCOUNT, KEY_SERVICE};
    use crate::claude_key_file::LegacyRead;

    pub fn read() -> LegacyRead {
        match keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).and_then(|e| e.get_password()) {
            Ok(k) if !k.trim().is_empty() => LegacyRead::Found(k),
            Ok(_) | Err(keyring::Error::NoEntry) => LegacyRead::Missing,
            Err(_) => LegacyRead::Failed,
        }
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    pub fn write(key: &str) -> Result<(), String> {
        keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT)
            .and_then(|e| e.set_password(key))
            .map_err(|_| "Couldn't save the key in the system's secure storage.".to_string())
    }

    pub fn delete() -> Result<(), String> {
        match keyring::Entry::new(KEY_SERVICE, KEY_ACCOUNT).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err("Couldn't remove the key from the system's secure storage.".into()),
        }
    }
}

/// Windows: Credential Manager (no prompts).
#[cfg(not(target_os = "macos"))]
mod system_store {
    use crate::claude_key_file::LegacyRead;

    /// The key, and whether that answer can be kept for the launch (a failed read is retried).
    pub fn load() -> (Option<String>, bool) {
        match super::os_store::read() {
            LegacyRead::Found(k) => (Some(k.trim().to_string()), true),
            LegacyRead::Missing => (None, true),
            LegacyRead::Failed => (None, false),
        }
    }

    pub fn save(key: &str) -> Result<(), String> {
        super::os_store::write(key)
    }

    pub fn delete() -> Result<(), String> {
        super::os_store::delete()
    }
}

/// macOS: Wren's private key file; the Keychain is only read to move an old key out of it.
#[cfg(target_os = "macos")]
mod system_store {
    use crate::claude_key_file::KeyFile;

    fn isolated_home() -> bool {
        std::env::var_os("WREN_HOME").is_some()
    }

    fn file() -> KeyFile {
        KeyFile::new(crate::paths::Paths::resolve().roaming)
    }

    /// Always kept for the launch: a denied Keychain prompt must not come back on every dictation.
    pub fn load() -> (Option<String>, bool) {
        let f = file();
        if isolated_home() {
            return (f.read(), true);
        }
        let key = f.get_or_move(super::os_store::read, || {
            if super::os_store::delete().is_err() {
                tracing::warn!("couldn't delete the old Claude key from the Keychain");
            }
        });
        (key, true)
    }

    pub fn save(key: &str) -> Result<(), String> {
        let f = file();
        let old_copy = !isolated_home() && !f.moved();
        f.write(key).map_err(|_| "Couldn't save the key in Wren's folder.".to_string())?;
        if old_copy && super::os_store::delete().is_err() {
            tracing::warn!("couldn't delete the old Claude key from the Keychain");
        }
        Ok(())
    }

    pub fn delete() -> Result<(), String> {
        let f = file();
        let old_copy = !isolated_home() && !f.moved();
        f.remove().map_err(|_| "Couldn't remove the key from Wren's folder.".to_string())?;
        // The marker now stops Wren reading any Keychain copy; still try to delete it.
        if old_copy && super::os_store::delete().is_err() {
            tracing::warn!("couldn't delete the old Claude key from the Keychain");
        }
        Ok(())
    }
}

// ---- usage and cost ----

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

    /// A `usage` object from the Messages API.
    pub fn from_json(v: &Value) -> Usage {
        let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        }
    }
}

/// List prices, $/MTok: (input, output, cache read). Cache writes (5 min) cost 1.25 × input.
/// Haiku 5.5's rate is for prompts up to 100K tokens (clean-up prompts are far below that).
fn prices(model: &str) -> (f64, f64, f64) {
    if model.starts_with("claude-haiku") {
        (0.10, 0.50, 0.01)
    } else if model.starts_with("claude-sonnet") {
        (2.0, 10.0, 0.10)
    } else {
        (4.0, 20.0, 0.20)
    }
}

/// Estimated cost of one request at list prices.
pub fn cost_usd(model: &str, u: &Usage) -> f64 {
    let (i, o, r) = prices(model);
    (u.input as f64 * i + u.output as f64 * o + u.cache_read as f64 * r + u.cache_write as f64 * i * 1.25) / 1_000_000.0
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

    /// Short machine kind for the UI (`note-error` events) and logs.
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

// ---- HTTP ----

/// A blocking client that never follows redirects (the key travels in a custom header that a
/// redirect would carry along).
pub fn client(connect_timeout: Duration, timeout: Duration) -> Result<reqwest::blocking::Client, ClaudeError> {
    reqwest::blocking::Client::builder()
        .connect_timeout(connect_timeout)
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| ClaudeError::Network(e.to_string()))
}

/// POST `body` to `url` (the Messages API, or a fake in tests), within `timeout` when given (else
/// the client's). A non-2xx status is mapped to a [`ClaudeError`]; a timeout is
/// `Network("timed out")`.
pub fn post(
    client: &reqwest::blocking::Client,
    url: &str,
    key: &str,
    beta: Option<&str>,
    timeout: Option<Duration>,
    body: &Value,
) -> Result<reqwest::blocking::Response, ClaudeError> {
    let mut req = client
        .post(url)
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json");
    if let Some(b) = beta {
        req = req.header("anthropic-beta", b);
    }
    if let Some(t) = timeout {
        req = req.timeout(t);
    }
    let resp = req
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn haiku_is_priced_on_its_own_card() {
        let u = Usage { input: 1_000_000, output: 1_000_000, cache_read: 1_000_000, cache_write: 1_000_000 };
        let haiku = cost_usd("claude-haiku-5-5", &u);
        assert!((haiku - (0.10 + 0.50 + 0.01 + 0.125)).abs() < 1e-9, "{haiku}");
        let sonnet = cost_usd("claude-sonnet-5-5", &Usage { input: 1_000_000, ..Default::default() });
        assert!((sonnet - 2.0).abs() < 1e-9);
    }

    #[test]
    fn usage_reads_the_api_field_names() {
        let v = serde_json::json!({"input_tokens": 5, "output_tokens": 7, "cache_read_input_tokens": 600, "cache_creation_input_tokens": 0});
        assert_eq!(Usage::from_json(&v), Usage { input: 5, output: 7, cache_read: 600, cache_write: 0 });
    }

    #[test]
    fn cache_reads_once_and_tracks_set_and_remove() {
        let c = KeyCache::new();
        let mut loads = 0;
        assert_eq!(
            c.get_or_load(|| {
                loads += 1;
                (Some("sk-ant-first".into()), true)
            })
            .as_deref(),
            Some("sk-ant-first")
        );
        assert_eq!(c.get_or_load(|| panic!("served from cache")).as_deref(), Some("sk-ant-first"));
        assert_eq!(loads, 1);

        // Failed save leaves the cached key untouched.
        assert!(c.set("sk-ant-bad".into(), |_| Err("disk full".into())).is_err());
        assert_eq!(c.get_or_load(|| panic!("still cached")).as_deref(), Some("sk-ant-first"));

        c.set("sk-ant-second".into(), |_| Ok(())).unwrap();
        assert_eq!(c.get_or_load(|| panic!("updated in cache")).as_deref(), Some("sk-ant-second"));

        // Failed delete leaves the cached key untouched.
        assert!(c.remove(|| Err("locked".into())).is_err());
        assert_eq!(c.get_or_load(|| panic!("still cached")).as_deref(), Some("sk-ant-second"));

        c.remove(|| Ok(())).unwrap();
        assert_eq!(c.get_or_load(|| panic!("absence is cached after remove")), None);
    }

    #[test]
    fn cache_retries_unsettled_failure_but_keeps_settled_none() {
        let win = KeyCache::new();
        assert_eq!(win.get_or_load(|| (None, false)), None);
        assert_eq!(win.get_or_load(|| (Some("sk-ant-retry".into()), true)).as_deref(), Some("sk-ant-retry"));

        let mac_denied = KeyCache::new();
        assert_eq!(mac_denied.get_or_load(|| (None, true)), None);
        assert_eq!(mac_denied.get_or_load(|| panic!("denied read must stay cached for the launch")), None);
    }

    #[test]
    fn cache_serialises_concurrent_loads() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        let cache = Arc::new(KeyCache::new());
        let loads = Arc::new(AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let c1 = Arc::clone(&cache);
        let l1 = Arc::clone(&loads);
        let t1 = std::thread::spawn(move || {
            c1.get_or_load(|| {
                l1.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                (Some("sk-ant-once".into()), true)
            })
        });

        entered_rx.recv().unwrap();
        let c2 = Arc::clone(&cache);
        let l2 = Arc::clone(&loads);
        let t2 = std::thread::spawn(move || {
            c2.get_or_load(|| {
                l2.fetch_add(1, Ordering::SeqCst);
                (Some("sk-ant-twice".into()), true)
            })
        });

        // Give t2 time to queue behind the lock before releasing t1.
        std::thread::sleep(Duration::from_millis(20));
        release_tx.send(()).unwrap();

        assert_eq!(t1.join().unwrap().as_deref(), Some("sk-ant-once"));
        assert_eq!(t2.join().unwrap().as_deref(), Some("sk-ant-once"));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }
}
