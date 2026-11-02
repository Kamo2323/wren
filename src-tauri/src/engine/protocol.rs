//! Protocol v1 between Wren and the speech sidecar: JSON lines on stdio, one object per line, UTF-8
//! (spec §5). Mirrors `sidecar/wren_sidecar/protocol.py`.
//!
//! Privacy: only `Incoming::Result` carries dictated text. Its `Debug` output is redacted, and a line
//! that fails to parse is never logged (it could be a result).

use super::Segment;
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

/// Wren → sidecar.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Transcribe { id: String, audio_path: String, bias_terms: Vec<String>, prompt: String },
    Ping,
    Pong,
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    Oom,
    BadAudio,
    NotReady,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Caps {
    /// false from Wren's sidecar: bias terms go in the Glossary prompt only (benchmark, spec §5).
    pub hotwords: bool,
    pub prompt: bool,
    pub vad: bool,
}

/// Sidecar → Wren.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Incoming {
    Ready {
        engine: String,
        model: String,
        device: String,
        compute: String,
        vram_mb: u64,
        #[serde(default)]
        caps: Caps,
    },
    Result {
        id: String,
        text: String,
        speech_ms: u64,
        infer_ms: u64,
        #[serde(default)]
        segments: Vec<Segment>,
    },
    Error {
        id: Option<String>,
        code: ErrorCode,
        msg: String,
    },
    Log {
        level: LogLevel,
        msg: String,
    },
    Ping,
    Pong,
    Shutdown,
}

impl std::fmt::Debug for Incoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Incoming::Result { id, text, speech_ms, infer_ms, segments } => f
                .debug_struct("Result")
                .field("id", id)
                .field("text_chars", &text.chars().count())
                .field("speech_ms", speech_ms)
                .field("infer_ms", infer_ms)
                .field("segments", &segments.len())
                .finish(),
            Incoming::Ready { engine, model, device, compute, vram_mb, caps } => f
                .debug_struct("Ready")
                .field("engine", engine)
                .field("model", model)
                .field("device", device)
                .field("compute", compute)
                .field("vram_mb", vram_mb)
                .field("caps", caps)
                .finish(),
            Incoming::Error { id, code, msg } => {
                f.debug_struct("Error").field("id", id).field("code", code).field("msg", msg).finish()
            }
            Incoming::Log { level, msg } => f.debug_struct("Log").field("level", level).field("msg", msg).finish(),
            Incoming::Ping => f.write_str("Ping"),
            Incoming::Pong => f.write_str("Pong"),
            Incoming::Shutdown => f.write_str("Shutdown"),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ProtocolError {
    #[error("line is not a JSON object")]
    NotJson,
    #[error("unsupported protocol version")]
    Version,
    #[error("unknown or malformed message")]
    Malformed,
}

#[derive(Serialize)]
struct Envelope<'a> {
    v: u32,
    #[serde(flatten)]
    msg: &'a Request,
}

/// One newline-terminated line.
pub fn encode(req: &Request) -> String {
    let mut line = serde_json::to_string(&Envelope { v: VERSION, msg: req }).expect("requests always serialise");
    line.push('\n');
    line
}

pub fn parse(line: &str) -> Result<Incoming, ProtocolError> {
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).map_err(|_| ProtocolError::NotJson)?;
    let obj = value.as_object().ok_or(ProtocolError::NotJson)?;
    if obj.get("v").and_then(|v| v.as_u64()) != Some(VERSION as u64) {
        return Err(ProtocolError::Version);
    }
    serde_json::from_value(value).map_err(|_| ProtocolError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcribe_serialises_exactly() {
        let line = encode(&Request::Transcribe {
            id: "d1".into(),
            audio_path: r"C:\a\b.wav".into(),
            bias_terms: vec!["Wren".into(), "Glaido".into()],
            prompt: "Hello.".into(),
        });
        assert_eq!(
            line,
            "{\"v\":1,\"type\":\"transcribe\",\"id\":\"d1\",\"audio_path\":\"C:\\\\a\\\\b.wav\",\"bias_terms\":[\"Wren\",\"Glaido\"],\"prompt\":\"Hello.\"}\n"
        );
    }

    #[test]
    fn bare_requests_serialise() {
        assert_eq!(encode(&Request::Ping), "{\"v\":1,\"type\":\"ping\"}\n");
        assert_eq!(encode(&Request::Pong), "{\"v\":1,\"type\":\"pong\"}\n");
        assert_eq!(encode(&Request::Shutdown), "{\"v\":1,\"type\":\"shutdown\"}\n");
    }

    #[test]
    fn non_ascii_stays_on_one_line() {
        let line = encode(&Request::Transcribe {
            id: "x".into(),
            audio_path: "a.wav".into(),
            bias_terms: vec!["café\nnaïve".into()],
            prompt: String::new(),
        });
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.contains("café"));
    }

    #[test]
    fn parses_ready() {
        let m = parse(r#"{"v":1,"type":"ready","engine":"faster-whisper","model":"large-v3-turbo","device":"cuda","compute":"int8_float16","vram_mb":1400,"caps":{"hotwords":false,"prompt":true,"vad":true}}"#).unwrap();
        assert_eq!(
            m,
            Incoming::Ready {
                engine: "faster-whisper".into(),
                model: "large-v3-turbo".into(),
                device: "cuda".into(),
                compute: "int8_float16".into(),
                vram_mb: 1400,
                caps: Caps { hotwords: false, prompt: true, vad: true },
            }
        );
    }

    #[test]
    fn parses_result_with_segments() {
        let m = parse(r#"{"v":1,"type":"result","id":"d1","text":"Hi there.","speech_ms":900,"infer_ms":120,"segments":[{"start":0.0,"end":1.2,"text":"Hi there.","no_speech_prob":0.02}]}"#).unwrap();
        let Incoming::Result { id, text, speech_ms, infer_ms, segments } = m else { panic!() };
        assert_eq!((id.as_str(), text.as_str(), speech_ms, infer_ms), ("d1", "Hi there.", 900, 120));
        assert_eq!(segments, vec![Segment { start: 0.0, end: 1.2, text: "Hi there.".into(), no_speech_prob: 0.02 }]);
    }

    #[test]
    fn parses_every_error_code_and_null_id() {
        for (s, code) in [
            ("OOM", ErrorCode::Oom),
            ("BAD_AUDIO", ErrorCode::BadAudio),
            ("NOT_READY", ErrorCode::NotReady),
            ("INTERNAL", ErrorCode::Internal),
        ] {
            let m = parse(&format!(r#"{{"v":1,"type":"error","id":"d1","code":"{s}","msg":"m"}}"#)).unwrap();
            assert_eq!(m, Incoming::Error { id: Some("d1".into()), code, msg: "m".into() });
        }
        let m = parse(r#"{"v":1,"type":"error","id":null,"code":"INTERNAL","msg":"bad request"}"#).unwrap();
        assert_eq!(m, Incoming::Error { id: None, code: ErrorCode::Internal, msg: "bad request".into() });
    }

    #[test]
    fn parses_log_ping_pong_shutdown() {
        assert_eq!(
            parse(r#"{"v":1,"type":"log","level":"warn","msg":"using small.en on CPU"}"#).unwrap(),
            Incoming::Log { level: LogLevel::Warn, msg: "using small.en on CPU".into() }
        );
        assert_eq!(parse("{\"v\":1,\"type\":\"ping\"}\n").unwrap(), Incoming::Ping);
        assert_eq!(parse(r#"{"v":1,"type":"pong"}"#).unwrap(), Incoming::Pong);
        assert_eq!(parse(r#"{"v":1,"type":"shutdown"}"#).unwrap(), Incoming::Shutdown);
    }

    #[test]
    fn rejects_bad_lines() {
        assert_eq!(parse("not json"), Err(ProtocolError::NotJson));
        assert_eq!(parse("[1]"), Err(ProtocolError::NotJson));
        assert_eq!(parse(r#"{"type":"ping"}"#), Err(ProtocolError::Version));
        assert_eq!(parse(r#"{"v":2,"type":"ping"}"#), Err(ProtocolError::Version));
        assert_eq!(parse(r#"{"v":1,"type":"teleport"}"#), Err(ProtocolError::Malformed));
        assert_eq!(parse(r#"{"v":1,"type":"result","id":"x"}"#), Err(ProtocolError::Malformed));
    }

    #[test]
    fn debug_never_shows_dictated_text() {
        let m = parse(r#"{"v":1,"type":"result","id":"d1","text":"my secret words","speech_ms":1,"infer_ms":1,"segments":[]}"#).unwrap();
        let dbg = format!("{m:?}");
        assert!(!dbg.contains("secret"), "{dbg}");
        assert!(dbg.contains("text_chars"));
    }
}
