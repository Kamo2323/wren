//! Meeting storage tests (plan Task 1). Every test works in its own temp folder.

use super::*;
use crate::meetings::{CaptureMode, Length, MeetingStatus, NewSegment, NoteRow, Source as MSource};
use chrono::Duration;

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("wren-meet-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn meetings(&self) -> PathBuf {
        let m = self.0.join("meetings");
        std::fs::create_dir_all(&m).unwrap();
        m
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open() -> (TempDir, Store) {
    let dir = TempDir::new();
    let store = Store::open(&dir.0.join("db").join("wren.db")).unwrap();
    (dir, store)
}

fn seg(meeting: &str, source: MSource, speaker: Option<&str>, start: u64, text: &str) -> NewSegment {
    NewSegment {
        meeting_id: meeting.to_string(),
        source,
        speaker: speaker.map(str::to_string),
        confidence: 0.9,
        muted: false,
        start_ms: start,
        end_ms: start + 1_000,
        text: text.to_string(),
    }
}

fn note(meeting: &str, length: Length, md: &str, cost: f64, at: DateTime<Utc>) -> NoteRow {
    NoteRow {
        meeting_id: meeting.to_string(),
        length,
        markdown: md.to_string(),
        model: "claude-opus-5-5".into(),
        input_tokens: 1000,
        output_tokens: 200,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: cost,
        created_at_utc: ts(at),
        stale: false,
    }
}

fn end(store: &Store, id: &str, ended: DateTime<Utc>) {
    store.meeting_set_status(id, MeetingStatus::Ended, Some(ended), 60_000).unwrap();
}

fn audio_files(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("me.wav"), b"RIFF").unwrap();
    std::fs::write(dir.join("them.wav"), b"RIFF").unwrap();
}

#[test]
fn create_list_get_roundtrip() {
    let (_d, s) = open();
    let a = s.meeting_create("Weekly sync", CaptureMode::TeamsOnly).unwrap();
    let b = s.meeting_create("Supplier call", CaptureMode::MicOnly).unwrap();
    assert_eq!(a.status, MeetingStatus::Recording);
    assert!(a.audio_available);
    let list = s.meeting_list().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].id, b.id, "newest first");
    let got = s.meeting_get(&a.id).unwrap().unwrap();
    assert_eq!(got.title, "Weekly sync");
    assert_eq!(got.capture_mode, CaptureMode::TeamsOnly);
    s.meeting_update(&a.id, Some("Renamed"), None, Some(&["Sarah Jones".to_string()]), Some("Teams Me")).unwrap();
    let got = s.meeting_get(&a.id).unwrap().unwrap();
    assert_eq!(got.title, "Renamed");
    assert_eq!(got.participants, vec!["Sarah Jones".to_string()]);
    assert_eq!(got.teams_self_name.as_deref(), Some("Teams Me"));
    assert!(s.meeting_get("nope").unwrap().is_none());
}

#[test]
fn segments_ordered_by_start_ms() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    s.segment_insert(&seg(&m.id, MSource::Them, Some("Sarah"), 5_000, "later")).unwrap();
    let first = s.segment_insert(&seg(&m.id, MSource::Me, Some("Maximilian"), 1_000, "earlier")).unwrap();
    assert_eq!(first.text, "earlier");
    let all = s.segments(&m.id).unwrap();
    assert_eq!(all.iter().map(|x| x.text.as_str()).collect::<Vec<_>>(), vec!["earlier", "later"]);
    assert_eq!(all[1].source, MSource::Them);
}

#[test]
fn rename_one_vs_all_marks_notes_stale() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    let one = s.segment_insert(&seg(&m.id, MSource::Them, None, 1_000, "a")).unwrap();
    s.segment_insert(&seg(&m.id, MSource::Them, None, 2_000, "b")).unwrap();
    s.segment_insert(&seg(&m.id, MSource::Them, Some("Tom"), 3_000, "c")).unwrap();
    s.note_upsert(&note(&m.id, Length::Medium, "# Summary", 0.1, Utc::now())).unwrap();

    assert_eq!(s.segment_rename(&m.id, Some(one.id), None, "Sarah").unwrap(), 1);
    assert!(s.notes(&m.id).unwrap()[0].stale);
    let segs = s.segments(&m.id).unwrap();
    assert_eq!(segs[0].speaker.as_deref(), Some("Sarah"));
    assert!(segs[0].renamed);
    assert_eq!(segs[1].speaker, None);

    assert_eq!(s.segment_rename(&m.id, None, Some("Tom"), "Tom Reid").unwrap(), 1);
    assert_eq!(s.segment_rename(&m.id, None, None, "Sarah").unwrap(), 1, "unnamed lines");
    let names: Vec<_> = s.segments(&m.id).unwrap().into_iter().map(|x| x.speaker.unwrap()).collect();
    assert_eq!(names, vec!["Sarah", "Sarah", "Tom Reid"]);
}

#[test]
fn editing_notes_marks_notes_stale() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    s.note_upsert(&note(&m.id, Length::Short, "x", 0.1, Utc::now())).unwrap();
    s.meeting_update(&m.id, Some("title only"), None, None, None).unwrap();
    assert!(!s.notes(&m.id).unwrap()[0].stale, "a title edit doesn't stale notes");
    s.meeting_update(&m.id, None, Some("{\"type\":\"doc\"}"), None, None).unwrap();
    assert!(s.notes(&m.id).unwrap()[0].stale);
    assert_eq!(s.meeting_get(&m.id).unwrap().unwrap().my_notes, "{\"type\":\"doc\"}");
    // A fresh upsert clears stale.
    s.note_upsert(&note(&m.id, Length::Short, "y", 0.1, Utc::now())).unwrap();
    let n = s.notes(&m.id).unwrap();
    assert_eq!(n.len(), 1);
    assert!(!n[0].stale);
    assert_eq!(n[0].markdown, "y");
}

#[test]
fn mark_interrupted_only_touches_recording_and_paused() {
    let (_d, s) = open();
    let rec = s.meeting_create("rec", CaptureMode::TeamsOnly).unwrap();
    let paused = s.meeting_create("paused", CaptureMode::TeamsOnly).unwrap();
    s.meeting_set_status(&paused.id, MeetingStatus::Paused, None, 0).unwrap();
    let ended = s.meeting_create("ended", CaptureMode::TeamsOnly).unwrap();
    end(&s, &ended.id, Utc::now());
    s.segment_insert(&seg(&rec.id, MSource::Me, None, 0, "kept")).unwrap();
    assert_eq!(s.mark_interrupted().unwrap(), 2);
    assert_eq!(s.meeting_get(&rec.id).unwrap().unwrap().status, MeetingStatus::Interrupted);
    assert!(s.meeting_get(&rec.id).unwrap().unwrap().ended_at_utc.is_some());
    assert_eq!(s.meeting_get(&paused.id).unwrap().unwrap().status, MeetingStatus::Interrupted);
    assert_eq!(s.meeting_get(&ended.id).unwrap().unwrap().status, MeetingStatus::Ended);
    assert_eq!(s.segments(&rec.id).unwrap().len(), 1);
}

#[test]
fn sweep_deletes_audio_after_24h_not_before() {
    let (d, s) = open();
    let dir = d.meetings();
    let now = Utc::now();
    let young = s.meeting_create("young", CaptureMode::TeamsOnly).unwrap();
    let old = s.meeting_create("old", CaptureMode::TeamsOnly).unwrap();
    end(&s, &young.id, now - Duration::hours(23) - Duration::minutes(59));
    end(&s, &old.id, now - Duration::hours(24) - Duration::minutes(1));
    audio_files(&dir.join(&young.id));
    audio_files(&dir.join(&old.id));
    s.segment_insert(&seg(&old.id, MSource::Me, None, 0, "kept")).unwrap();
    s.note_upsert(&note(&old.id, Length::Medium, "kept", 0.1, now)).unwrap();

    assert_eq!(s.meeting_sweep(now, &dir).unwrap(), 1);
    assert!(dir.join(&young.id).join("me.wav").exists());
    assert!(!dir.join(&old.id).join("me.wav").exists());
    assert!(!dir.join(&old.id).join("them.wav").exists());
    let o = s.meeting_get(&old.id).unwrap().unwrap();
    assert!(!o.audio_available);
    assert!(s.meeting_get(&young.id).unwrap().unwrap().audio_available);
    assert_eq!(s.segments(&old.id).unwrap().len(), 1);
    assert_eq!(s.notes(&old.id).unwrap().len(), 1);
    // Idempotent.
    assert_eq!(s.meeting_sweep(now, &dir).unwrap(), 0);
}

#[test]
fn sweep_deletes_call_diagnostics_with_the_audio() {
    let (d, s) = open();
    let dir = d.meetings();
    let now = Utc::now();
    let young = s.meeting_create("young", CaptureMode::TeamsOnly).unwrap();
    let old = s.meeting_create("old", CaptureMode::TeamsOnly).unwrap();
    end(&s, &young.id, now - Duration::hours(23));
    end(&s, &old.id, now - Duration::hours(24) - Duration::minutes(1));
    let diag = crate::meetings::diagnostics::FILE_NAME;
    for id in [&young.id, &old.id] {
        audio_files(&dir.join(id));
        std::fs::write(dir.join(id).join(diag), b"{}").unwrap();
    }
    assert_eq!(s.meeting_sweep(now, &dir).unwrap(), 1);
    assert!(dir.join(&young.id).join(diag).exists(), "kept as long as the audio");
    assert!(!dir.join(&old.id).join(diag).exists());
    assert!(!s.meeting_get(&old.id).unwrap().unwrap().audio_available);
}

#[test]
fn sweep_removes_orphan_folders() {
    let (d, s) = open();
    let dir = d.meetings();
    let kept = s.meeting_create("kept", CaptureMode::TeamsOnly).unwrap();
    audio_files(&dir.join(&kept.id));
    audio_files(&dir.join("orphan-id"));
    // Orphans must be old enough; sweep as if two hours from now.
    s.meeting_sweep(Utc::now() + Duration::hours(2), &dir).unwrap();
    assert!(!dir.join("orphan-id").exists());
    assert!(dir.join(&kept.id).join("me.wav").exists(), "a recording meeting keeps its audio");
}

#[test]
fn delete_removes_rows_and_folder() {
    let (d, s) = open();
    let dir = d.meetings();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    audio_files(&dir.join(&m.id));
    s.segment_insert(&seg(&m.id, MSource::Me, None, 0, "x")).unwrap();
    s.note_upsert(&note(&m.id, Length::Long, "x", 0.1, Utc::now())).unwrap();
    s.meeting_delete(&m.id, &dir).unwrap();
    assert!(s.meeting_get(&m.id).unwrap().is_none());
    assert!(s.segments(&m.id).unwrap().is_empty());
    assert!(s.notes(&m.id).unwrap().is_empty());
    assert!(!dir.join(&m.id).exists());
}

#[test]
fn cost_since_sums_only_after_cutoff() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    let now = Utc::now();
    s.note_upsert(&note(&m.id, Length::Short, "a", 0.10, now - Duration::days(40))).unwrap();
    s.note_upsert(&note(&m.id, Length::Medium, "b", 0.25, now - Duration::days(1))).unwrap();
    s.note_upsert(&note(&m.id, Length::Long, "c", 0.50, now)).unwrap();
    let (usd, n) = s.notes_cost_since(now - Duration::days(30)).unwrap();
    assert!((usd - 0.75).abs() < 1e-9, "{usd}");
    assert_eq!(n, 2);
}

#[test]
fn preview_prefers_medium_summary_then_first_segment() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    s.segment_insert(&seg(&m.id, MSource::Them, Some("Sarah"), 0, "Hello everyone")).unwrap();
    assert_eq!(s.meeting_list().unwrap()[0].preview.as_deref(), Some("Hello everyone"));
    s.note_upsert(&note(&m.id, Length::Medium, "## Summary\n\n- Rollout moved to Thursday.\n", 0.1, Utc::now()))
        .unwrap();
    assert_eq!(s.meeting_list().unwrap()[0].preview.as_deref(), Some("Rollout moved to Thursday."));
    // The headline box is the best preview, without its label or bold marks.
    s.note_upsert(&note(&m.id, Length::Medium, "> **In short:** Rollout moves to **Thursday**.\n\n## Summary\n", 0.1, Utc::now()))
        .unwrap();
    assert_eq!(s.meeting_list().unwrap()[0].preview.as_deref(), Some("Rollout moves to Thursday."));
}

#[test]
fn migration_upgrades_existing_db_without_touching_transcriptions() {
    let dir = TempDir::new();
    let path = dir.0.join("wren.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema::MIGRATIONS[0]).unwrap();
        conn.execute_batch("PRAGMA user_version = 1").unwrap();
        conn.execute(
            "INSERT INTO transcriptions (id, created_at_utc, local_date, local_hour, mode, duration_ms, speech_ms, \
             raw_text, tidied_text, final_text, words, model, device, timings_json, target_process, paste_outcome) \
             VALUES ('t1', '2026-11-01T10:00:00.000Z', '2026-11-01', 10, 'hold', 500, 400, 'hi', 'hi', 'hi', 1, \
             'm', 'cuda', '{}', 'notepad.exe', 'pasted')",
            [],
        )
        .unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert!(s.get("t1").unwrap().is_some());
    assert!(s.meeting_list().unwrap().is_empty());
}

// ---------------- council round 1 ----------------

#[test]
fn sweep_keeps_audio_flag_when_a_file_cannot_be_deleted() {
    let (d, s) = open();
    let dir = d.meetings();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    end(&s, &m.id, Utc::now() - Duration::hours(30));
    // A folder named me.wav can't be removed as a file: the delete "fails".
    std::fs::create_dir_all(dir.join(&m.id).join("me.wav").join("x")).unwrap();
    assert_eq!(s.meeting_sweep(Utc::now(), &dir).unwrap(), 0);
    assert!(s.meeting_get(&m.id).unwrap().unwrap().audio_available, "retried next sweep, not marked gone");
}

#[test]
fn note_for_a_deleted_meeting_is_not_saved() {
    let (d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    let started = Utc::now();
    s.meeting_delete(&m.id, &d.meetings()).unwrap();
    let saved = s.note_save_if_current(&note(&m.id, Length::Medium, "x", 0.1, Utc::now()), started).unwrap();
    assert!(!saved);
    assert!(s.notes(&m.id).unwrap().is_empty());
}

#[test]
fn note_is_stale_when_inputs_changed_while_it_was_written() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    let started = Utc::now();
    std::thread::sleep(std::time::Duration::from_millis(5));
    s.meeting_update(&m.id, None, Some("{\"type\":\"doc\"}"), None, None).unwrap();
    assert!(s.note_save_if_current(&note(&m.id, Length::Medium, "x", 0.1, Utc::now()), started).unwrap());
    assert!(s.notes(&m.id).unwrap()[0].stale, "edits during generation mark it stale");
    let later = Utc::now();
    assert!(s.note_save_if_current(&note(&m.id, Length::Medium, "y", 0.1, Utc::now()), later).unwrap());
    assert!(!s.notes(&m.id).unwrap()[0].stale);
}

#[test]
fn regenerated_notes_still_count_in_spend() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    let t = Utc::now();
    s.note_save_if_current(&note(&m.id, Length::Medium, "a", 0.10, t), t).unwrap();
    s.note_save_if_current(&note(&m.id, Length::Medium, "b", 0.20, t), t).unwrap();
    let (usd, n) = s.notes_cost_since(t - Duration::hours(1)).unwrap();
    assert!((usd - 0.30).abs() < 1e-9, "{usd}");
    assert_eq!(n, 2);
}

#[test]
fn clean_up_spend_has_its_own_counter() {
    let (_d, s) = open();
    let t = Utc::now();
    s.log_cleanup_spend(t - Duration::days(40), "claude-haiku-5-5", 0.50).unwrap();
    s.log_cleanup_spend(t, "claude-haiku-5-5", 0.0002).unwrap();
    s.log_cleanup_spend(t, "claude-sonnet-5-5", 0.004).unwrap();
    let (usd, n) = s.cleanup_cost_since(t - Duration::hours(1)).unwrap();
    assert!((usd - 0.0042).abs() < 1e-9, "{usd}");
    assert_eq!(n, 2);
    assert_eq!(s.notes_cost_since(t - Duration::hours(1)).unwrap(), (0.0, 0), "notes are counted apart");
}

#[test]
fn rename_all_by_name_never_touches_my_lines() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    s.segment_insert(&seg(&m.id, MSource::Me, Some("Maximilian"), 0, "mine")).unwrap();
    s.segment_insert(&seg(&m.id, MSource::Them, Some("Maximilian"), 1_000, "theirs")).unwrap();
    assert_eq!(s.segment_rename(&m.id, None, Some("Maximilian"), "Max Other").unwrap(), 1);
    let segs = s.segments(&m.id).unwrap();
    assert_eq!(segs[0].speaker.as_deref(), Some("Maximilian"));
    assert_eq!(segs[1].speaker.as_deref(), Some("Max Other"));
}

#[test]
fn segment_for_a_deleted_meeting_is_rejected() {
    let (d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    s.meeting_delete(&m.id, &d.meetings()).unwrap();
    assert!(s.segment_insert(&seg(&m.id, MSource::Me, None, 0, "late")).is_err());
    assert!(s.segments(&m.id).unwrap().is_empty());
}

#[test]
fn interrupted_meetings_with_audio_are_listed_for_repair() {
    let (_d, s) = open();
    let m = s.meeting_create("M", CaptureMode::TeamsOnly).unwrap();
    s.mark_interrupted().unwrap();
    assert_eq!(s.interrupted_with_audio().unwrap(), vec![m.id]);
}
