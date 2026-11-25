use super::*;
use crate::meetings::session::ActiveMeeting;
use crate::meetings::{CaptureMode, MeetingStatus};

#[test]
fn terminal_note_event_clears_the_generating_snapshot_before_the_run_drops() {
    let id = uuid::Uuid::new_v4().to_string();
    let length = Length::Short;
    GENERATING.lock().push((id.clone(), length));
    let run = Generating::new(id.clone(), length);
    run.publish(NoteEvent::Delta { meeting_id: id.clone(), length, text: "draft".into() }, |_| {
        assert_eq!(meeting_generating(id.clone()), vec![length]);
    });
    run.publish(NoteEvent::Error { meeting_id: id.clone(), length, kind: "api".into(), message: "failed".into() }, |_| {
        // A concurrent snapshot may wait for the terminal event to be queued, but can't read
        // the old run after receiving that event.
        if let Some(generating) = GENERATING.try_lock() {
            assert!(!generating.contains(&(id.clone(), length)));
        }
    });
    assert!(meeting_generating(id).is_empty());
    drop(run);
}

#[test]
fn settings_defaults_meeting_name_and_model() {
    let s = crate::settings::Settings::default();
    assert_eq!(s.meeting_name, "Maximilian");
    assert_eq!(s.claude_model, "claude-opus-5-5");
    // An older settings.json without the new fields loads with the defaults.
    let old: crate::settings::Settings = serde_json::from_str(r#"{"version":1,"typingWpm":40}"#).unwrap();
    assert_eq!(old.meeting_name, "Maximilian");
    assert_eq!(old.claude_model, "claude-opus-5-5");
}

#[test]
fn settings_roundtrip_keeps_meeting_fields() {
    let mut s = crate::settings::Settings::default();
    s.meeting_name = "Max".into();
    s.claude_model = "claude-sonnet-5-5".into();
    let json = serde_json::to_string(&s).unwrap();
    assert!(json.contains("\"meetingName\":\"Max\""));
    assert!(json.contains("\"claudeModel\":\"claude-sonnet-5-5\""));
    let back: crate::settings::Settings = serde_json::from_str(&json).unwrap();
    assert_eq!(back.meeting_name, "Max");
}

#[test]
fn active_meeting_serialises_camel_case() {
    let a = ActiveMeeting {
        id: "m1".into(),
        status: MeetingStatus::Recording,
        started_at_utc: "2026-11-24T10:00:00.000Z".into(),
        elapsed_ms: 1234,
        capture_mode: CaptureMode::TeamsOnly,
        catching_up: false,
        speaking_me: true,
        speaking_them: false,
    };
    let v = serde_json::to_value(&a).unwrap();
    for k in ["id", "status", "startedAtUtc", "elapsedMs", "captureMode", "catchingUp", "speakingMe", "speakingThem"] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(v["captureMode"], "teams_only");
    assert_eq!(v["status"], "recording");
}

#[test]
fn meeting_detail_serialises_camel_case_and_hides_teams_name() {
    let dir = std::env::temp_dir().join(format!("wren-cmd-{}", uuid::Uuid::new_v4()));
    let store = crate::store::Store::open(&dir.join("w.db")).unwrap();
    let m = store.meeting_create("T", CaptureMode::MicOnly).unwrap();
    store.meeting_update(&m.id, None, None, None, Some("Secret Teams Name")).unwrap();
    let d = detail(&store, &m.id).unwrap().unwrap();
    let v = serde_json::to_value(&d).unwrap();
    assert!(v["meeting"].get("startedAtUtc").is_some());
    assert!(v["meeting"].get("audioAvailable").is_some());
    assert!(v.get("segments").is_some() && v.get("notes").is_some());
    assert!(!v.to_string().contains("Secret Teams Name"), "own Teams name never reaches the UI");
    drop(store);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn meeting_events_serialise_camel_case_fields() {
    use crate::meetings::session::MeetingEvent;
    use crate::meetings::Source;
    let v = serde_json::to_value(MeetingEvent::Speaking { meeting_id: "m".into(), source: Source::Them, on: true }).unwrap();
    assert_eq!(v["kind"], "speaking");
    assert_eq!(v["meetingId"], "m");
    assert_eq!(v["source"], "them");
    let v = serde_json::to_value(MeetingEvent::Title { meeting_id: "m".into(), title: "T".into() }).unwrap();
    assert_eq!(v["meetingId"], "m");
    let v = serde_json::to_value(crate::meetings::enhance::NoteEvent::Delta {
        meeting_id: "m".into(),
        length: crate::meetings::Length::Short,
        text: "x".into(),
    })
    .unwrap();
    assert_eq!(v["event"], "delta");
    assert_eq!(v["meetingId"], "m");
    assert_eq!(v["length"], "short");
}

#[test]
fn recording_dot_paints_bottom_right_red_and_keeps_rest() {
    let (w, h) = (32u32, 32u32);
    let mut rgba = vec![10u8; (w * h * 4) as usize];
    recording_dot(&mut rgba, w, h);
    let px = |x: u32, y: u32| &rgba[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
    let c = px(w - 7, h - 7);
    assert!(c[0] > 200 && c[1] < 80 && c[2] < 80 && c[3] == 255, "{c:?}");
    assert_eq!(px(2, 2), &[10, 10, 10, 10], "top-left untouched");
}

#[test]
fn clip_data_url_cuts_the_requested_range() {
    let dir = std::env::temp_dir().join(format!("wren-clip-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("them.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 16_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&p, spec).unwrap();
    for i in 0..(16_000 * 10) {
        w.write_sample((i % 100) as i16).unwrap();
    }
    w.finalize().unwrap();
    let url = clip_data_url(&p, 2_000, 3_000).unwrap().unwrap();
    assert!(url.starts_with("data:audio/wav;base64,"));
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD.decode(&url["data:audio/wav;base64,".len()..]).unwrap();
    let r = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
    let secs = r.duration() as f64 / 16_000.0;
    assert!((secs - 1.5).abs() < 0.01, "range plus 500 ms tail: {secs}");
    // Past the end: clamped; missing file: None.
    assert!(clip_data_url(&p, 9_800, 20_000).unwrap().is_some());
    assert!(clip_data_url(&dir.join("nope.wav"), 0, 1_000).unwrap().is_none());
    let _ = std::fs::remove_dir_all(dir);
}
