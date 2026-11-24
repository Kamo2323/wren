use super::*;

#[test]
fn request_shape_matches_constraints() {
    let body = build_request("claude-opus-5-5", "SYSTEM", "CONTEXT", "INSTRUCTION");
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 16000);
    assert_eq!(body["output_config"]["effort"], "medium");
    assert_eq!(body["fallbacks"], "default");
    for k in ["thinking", "temperature", "top_p", "top_k"] {
        assert!(body.get(k).is_none(), "{k} must not be sent");
    }
    assert_eq!(body["system"][0]["text"], "SYSTEM");
    let content = &body["messages"][0]["content"];
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(content[0]["text"], "CONTEXT");
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(content[1]["text"], "INSTRUCTION");
    assert!(content[1].get("cache_control").is_none(), "only the context block is cached");
}

#[test]
fn headers_are_pinned() {
    assert_eq!(API_URL, "https://api.anthropic.com/v1/messages");
    assert_eq!(API_VERSION, "2026-10-21");
    assert_eq!(BETA, "server-side-fallback-2026-07-01");
}

fn feed(chunks: &[&str]) -> Vec<SseEvent> {
    let mut st = SseState::default();
    chunks.iter().flat_map(|c| parse_sse(c, &mut st)).collect()
}

#[test]
fn sse_text_deltas_concatenate_across_split_chunks() {
    let ev = feed(&[
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel",
        "lo\"}}\n\nevent: content_block_delta\r\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\" world\"}}\r\n\r\n",
    ]);
    let text: String = ev.iter().filter_map(|e| if let SseEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect();
    assert_eq!(text, "Hello world");
}

#[test]
fn sse_usage_from_message_start_and_delta() {
    let ev = feed(&[
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1200,\"cache_creation_input_tokens\":15000,\"cache_read_input_tokens\":0,\"output_tokens\":1}}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":812}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    ]);
    let mut usage = Usage::default();
    let mut stop = None;
    for e in ev {
        match e {
            SseEvent::Usage(u) => usage.merge(&u),
            SseEvent::Stop(s) => stop = Some(s),
            _ => {}
        }
    }
    assert_eq!(usage, Usage { input: 1200, output: 812, cache_read: 0, cache_write: 15000 });
    assert_eq!(stop.as_deref(), Some("end_turn"));
}

#[test]
fn sse_ignores_thinking_fallback_and_ping() {
    let ev = feed(&[
        "event: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"hmm\"}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"fallback\"}}\n\n",
        "event: something_new\ndata: {\"type\":\"something_new\"}\n\n",
    ]);
    assert!(ev.iter().all(|e| matches!(e, SseEvent::Ignored)), "{ev:?}");
}

#[test]
fn sse_error_event_maps_overloaded() {
    let ev = feed(&["event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n"]);
    assert_eq!(ev, vec![SseEvent::Error { kind: "overloaded_error".into(), message: "Overloaded".into() }]);
    assert_eq!(error_from_kind("overloaded_error", "x"), ClaudeError::Overloaded);
    assert_eq!(error_from_kind("rate_limit_error", "x"), ClaudeError::RateLimited);
    assert_eq!(error_from_kind("authentication_error", "x"), ClaudeError::Unauthorized);
}

#[test]
fn stop_reason_refusal_maps_refusal() {
    assert_eq!(stop_error("refusal"), Some(ClaudeError::Refusal));
    assert_eq!(stop_error("end_turn"), None);
    assert_eq!(stop_error("max_tokens"), None, "a long note that hit the cap is still kept");
}

#[test]
fn http_status_maps_to_errors() {
    assert_eq!(error_from_status(401, ""), ClaudeError::Unauthorized);
    assert_eq!(error_from_status(403, ""), ClaudeError::Unauthorized);
    assert_eq!(error_from_status(429, ""), ClaudeError::RateLimited);
    assert_eq!(error_from_status(529, ""), ClaudeError::Overloaded);
    assert!(matches!(error_from_status(400, "bad"), ClaudeError::Api(_)));
    assert!(error_from_status(500, "").retryable());
    assert!(!error_from_status(400, "").retryable());
}

#[test]
fn cost_opus_and_sonnet() {
    let u = Usage { input: 1_000_000, output: 100_000, cache_read: 1_000_000, cache_write: 1_000_000 };
    // Opus 5.5: $4 in, $20 out, $0.20 cache read, cache write 1.25 × input.
    let opus = cost_usd("claude-opus-5-5", &u);
    assert!((opus - (4.0 + 2.0 + 0.20 + 5.0)).abs() < 1e-9, "{opus}");
    let sonnet = cost_usd("claude-sonnet-5-5", &u);
    assert!((sonnet - (2.0 + 1.0 + 0.10 + 2.5)).abs() < 1e-9, "{sonnet}");
}

#[test]
fn model_label_for_footer() {
    assert_eq!(model_label("claude-opus-5-5"), "Claude Opus 5.5");
    assert_eq!(model_label("claude-sonnet-5-5"), "Claude Sonnet 5.5");
}

#[test]
fn secret_debug_redacted() {
    let s = Secret::new("sk-ant-api03-SUPERSECRET".into());
    let dbg = format!("{s:?}");
    assert!(!dbg.contains("SUPERSECRET"), "{dbg}");
    assert_eq!(s.expose(), "sk-ant-api03-SUPERSECRET");
}

#[test]
fn memory_keystore_roundtrip() {
    let ks = KeyStore::memory();
    assert!(ks.get().is_none());
    ks.set("sk-ant-test").unwrap();
    assert_eq!(ks.get().unwrap().expose(), "sk-ant-test");
    ks.remove().unwrap();
    assert!(ks.get().is_none());
}

#[test]
fn key_shape_checked_before_saving() {
    assert!(looks_like_key("sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789"));
    assert!(!looks_like_key(""));
    assert!(!looks_like_key("hello world"));
    assert!(!looks_like_key("sk-ant-short"));
}

#[test]
#[ignore = "live: calls the Anthropic API with the saved key"]
fn live_test_key() {
    let key = KeyStore::system().get().expect("a key saved in the OS credential store");
    test_key(key.expose()).expect("the key works");
}

#[test]
fn message_stop_is_reported() {
    let ev = feed(&["event: message_stop
data: {\"type\":\"message_stop\"}

"]);
    assert_eq!(ev, vec![SseEvent::MessageStop]);
}

#[test]
fn a_stream_without_message_stop_is_incomplete() {
    assert!(matches!(completion_error("end_turn", false), Some(ClaudeError::Network(_))));
    assert_eq!(completion_error("end_turn", true), None);
    assert_eq!(completion_error("refusal", true), Some(ClaudeError::Refusal));
    assert!(matches!(completion_error("", true), Some(ClaudeError::Network(_))));
}
