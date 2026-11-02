//! Store tests. Every test works in its own temp folder; none touch %LOCALAPPDATA%\Wren.

use super::*;
use crate::constants::RETENTION_HOURS;
use crate::dictionary::EntryKind;
use chrono::{
    Datelike, Duration, FixedOffset, LocalResult, NaiveDateTime, Offset, TimeZone, Timelike, Weekday,
};

/// A temp folder removed on drop.
struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("wren-store-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
    fn audio(&self) -> PathBuf {
        let a = self.0.join("audio");
        std::fs::create_dir_all(&a).unwrap();
        a
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

fn utc(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn tx(id: &str, at: DateTime<Utc>, text: &str, duration_ms: u64) -> NewTranscription {
    NewTranscription {
        id: id.into(),
        created_at_utc: at,
        mode: Mode::Hold,
        audio_path: None,
        duration_ms,
        speech_ms: duration_ms / 2,
        raw_text: text.to_lowercase(),
        tidied_text: text.into(),
        final_text: text.into(),
        model: "test-model".into(),
        device: "cpu".into(),
        timings_json: "{}".into(),
        target_process: "notepad.exe".into(),
        paste_outcome: PasteOutcome::Pasted,
        applied_rule_ids: vec![],
    }
}

fn utc_tz() -> FixedOffset {
    FixedOffset::east_opt(0).unwrap()
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .conn()
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

/// UK time with BST, without a time-zone database: BST runs from 01:00 UTC on the last Sunday
/// of March to 01:00 UTC on the last Sunday of October.
#[derive(Clone, Copy, Debug)]
struct London;

fn last_sunday(year: i32, month: u32) -> NaiveDate {
    let mut d = NaiveDate::from_ymd_opt(year, month + 1, 1).unwrap() - Duration::days(1);
    while d.weekday() != Weekday::Sun {
        d -= Duration::days(1);
    }
    d
}

impl London {
    fn offset_at_utc(utc: &NaiveDateTime) -> FixedOffset {
        let y = utc.year();
        let start = last_sunday(y, 3).and_hms_opt(1, 0, 0).unwrap();
        let end = last_sunday(y, 10).and_hms_opt(1, 0, 0).unwrap();
        let secs = if *utc >= start && *utc < end { 3600 } else { 0 };
        FixedOffset::east_opt(secs).unwrap()
    }
}

impl TimeZone for London {
    type Offset = FixedOffset;
    fn from_offset(_: &FixedOffset) -> Self {
        London
    }
    fn offset_from_local_date(&self, local: &NaiveDate) -> LocalResult<FixedOffset> {
        LocalResult::Single(Self::offset_at_utc(&local.and_hms_opt(12, 0, 0).unwrap()))
    }
    fn offset_from_local_datetime(&self, local: &NaiveDateTime) -> LocalResult<FixedOffset> {
        LocalResult::Single(Self::offset_at_utc(local))
    }
    fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
        Self::offset_at_utc(&utc.and_hms_opt(12, 0, 0).unwrap())
    }
    fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
        Self::offset_at_utc(utc).fix()
    }
}

// ---------- open / schema ----------

#[test]
fn open_creates_the_folder_migrates_and_sets_pragmas() {
    let (dir, store) = open();
    assert!(dir.0.join("db").join("wren.db").exists());
    let conn = store.conn().unwrap();
    assert_eq!(schema::user_version(&conn).unwrap(), schema::latest_version());
    let mode: String = conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    let secure: i64 = conn.query_row("PRAGMA secure_delete", [], |r| r.get(0)).unwrap();
    assert_eq!(secure, 1);
}

#[test]
fn reopening_keeps_data() {
    let dir = TempDir::new();
    let path = dir.0.join("wren.db");
    {
        let s = Store::open(&path).unwrap();
        s.insert_transcription_in(&tx("a", utc("2026-11-02T10:00:00Z"), "hello there", 1_000), &utc_tz())
            .unwrap();
    }
    let s = Store::open(&path).unwrap();
    assert_eq!(s.get("a").unwrap().unwrap().final_text, "hello there");
}

#[test]
fn a_database_from_a_newer_wren_is_refused() {
    let dir = TempDir::new();
    let path = dir.0.join("wren.db");
    Connection::open(&path).unwrap().execute_batch("PRAGMA user_version = 99").unwrap();
    assert!(matches!(Store::open(&path), Err(StoreError::TooNew { found: 99, .. })));
}

#[test]
fn an_unavailable_store_reports_its_reason_on_every_call() {
    let s = Store::unavailable("disk full");
    assert_eq!(s.unavailable_reason(), Some("disk full"));
    let err = s.recent(10).unwrap_err().to_string();
    assert!(err.contains("disk full"), "{err}");
    assert!(s.dictionary_list().is_err());
}

// ---------- insert + hourly totals ----------

#[test]
fn insert_writes_the_row_and_the_hourly_totals() {
    let (_d, store) = open();
    let tz = FixedOffset::east_opt(3600).unwrap(); // +01:00
    store.insert_transcription_in(&tx("a", utc("2026-11-02T09:30:00Z"), "one two three", 4_000), &tz).unwrap();
    store.insert_transcription_in(&tx("b", utc("2026-11-02T09:45:00Z"), "four five", 2_000), &tz).unwrap();
    let conn = store.conn().unwrap();
    let (date, hour, n, words, ms): (String, i64, i64, i64, i64) = conn
        .query_row("SELECT local_date, hour, dictations, words, speaking_ms FROM hourly_totals", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap();
    assert_eq!((date.as_str(), hour, n, words, ms), ("2026-11-02", 10, 2, 5, 6_000));
    let (ld, lh, w): (String, i64, i64) = conn
        .query_row("SELECT local_date, local_hour, words FROM transcriptions WHERE id = 'a'", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!((ld.as_str(), lh, w), ("2026-11-02", 10, 3));
}

#[test]
fn a_failed_hourly_update_rolls_back_the_transcription_too() {
    let (_d, store) = open();
    store
        .conn()
        .unwrap()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_hourly_insert BEFORE INSERT ON hourly_totals
               BEGIN SELECT RAISE(ABORT, 'simulated failure'); END;",
        )
        .unwrap();
    let res = store.insert_transcription_in(&tx("a", utc("2026-11-02T09:30:00Z"), "hello", 1_000), &utc_tz());
    assert!(res.is_err());
    assert_eq!(count(&store, "transcriptions"), 0);
    assert_eq!(count(&store, "hourly_totals"), 0);
}

#[test]
fn a_failed_transcription_insert_leaves_the_hourly_totals_alone() {
    let (_d, store) = open();
    let t = tx("dup", utc("2026-11-02T09:30:00Z"), "hello world", 1_000);
    store.insert_transcription_in(&t, &utc_tz()).unwrap();
    assert!(store.insert_transcription_in(&t, &utc_tz()).is_err()); // same id
    let words: i64 = store.conn().unwrap().query_row("SELECT words FROM hourly_totals", [], |r| r.get(0)).unwrap();
    assert_eq!((count(&store, "transcriptions"), words), (1, 2));
}

#[test]
fn rows_round_trip_with_camel_case_json() {
    let (_d, store) = open();
    let mut t = tx("a", utc("2026-11-02T09:30:00.250Z"), "Hello there", 1_500);
    t.mode = Mode::HandsFree;
    t.paste_outcome = PasteOutcome::NotPasted;
    t.applied_rule_ids = vec![3, 4];
    store.insert_transcription_in(&t, &utc_tz()).unwrap();
    let row = store.get("a").unwrap().unwrap();
    assert_eq!(
        row,
        TranscriptionRow {
            id: "a".into(),
            created_at_utc: "2026-11-02T09:30:00.250Z".into(),
            mode: Mode::HandsFree,
            duration_ms: 1_500,
            final_text: "Hello there".into(),
            raw_text: "hello there".into(),
            words: 2,
            target_process: "notepad.exe".into(),
            paste_outcome: PasteOutcome::NotPasted,
        }
    );
    let json = serde_json::to_value(&row).unwrap();
    assert_eq!(json["mode"], "handsFree");
    assert_eq!(json["pasteOutcome"], "notPasted");
    assert_eq!(json["createdAtUtc"], "2026-11-02T09:30:00.250Z");
    let stored: (String, String, String) = store
        .conn()
        .unwrap()
        .query_row("SELECT mode, paste_outcome, applied_rule_ids FROM transcriptions", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap();
    assert_eq!(stored, ("handsfree".into(), "not_pasted".into(), "[3,4]".into()));
    assert!(store.get("missing").unwrap().is_none());
}

#[test]
fn set_paste_outcome_updates_the_row() {
    let (_d, store) = open();
    store.insert_transcription_in(&tx("a", utc("2026-11-02T09:30:00Z"), "hi", 1_000), &utc_tz()).unwrap();
    store.set_paste_outcome("a", PasteOutcome::Copied).unwrap();
    assert_eq!(store.get("a").unwrap().unwrap().paste_outcome, PasteOutcome::Copied);
    assert!(matches!(store.set_paste_outcome("nope", PasteOutcome::Failed), Err(StoreError::NotFound)));
}

#[test]
fn recent_is_newest_first_and_limited() {
    let (_d, store) = open();
    for (i, at) in ["2026-11-02T09:00:00Z", "2026-11-02T11:00:00Z", "2026-11-02T10:00:00Z"].iter().enumerate() {
        store.insert_transcription_in(&tx(&format!("t{i}"), utc(at), "x", 1_000), &utc_tz()).unwrap();
    }
    let ids: Vec<String> = store.recent(2).unwrap().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, ["t1", "t2"]);
}

#[test]
fn search_is_a_case_insensitive_like_with_wildcards_escaped() {
    let (_d, store) = open();
    let tz = utc_tz();
    store.insert_transcription_in(&tx("q", utc("2026-11-02T09:00:00Z"), "Stand in the queue", 1_000), &tz).unwrap();
    store.insert_transcription_in(&tx("p", utc("2026-11-02T09:01:00Z"), "I am 100% sure", 1_000), &tz).unwrap();
    store.insert_transcription_in(&tx("u", utc("2026-11-02T09:02:00Z"), "an under_score", 1_000), &tz).unwrap();
    let ids = |q: &str| store.search(q).unwrap().into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(ids("QUEUE"), ["q"]);
    assert_eq!(ids("%"), ["p"]);
    assert_eq!(ids("_"), ["u"]);
    assert_eq!(ids("nothing like this"), Vec::<String>::new());
    assert_eq!(ids("  "), ["u", "p", "q"]);
}

// ---------- deletion and the sweep ----------

fn wav(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, b"RIFF").unwrap();
    p
}

#[test]
fn delete_transcription_removes_row_audio_and_events_but_keeps_totals() {
    let (d, store) = open();
    let audio = d.audio();
    let mut t = tx("a", utc("2026-11-02T09:00:00Z"), "Klode is great", 1_000);
    t.audio_path = Some(wav(&audio, "a.wav"));
    store.insert_transcription_in(&t, &utc_tz()).unwrap();
    store.learn(Source::LearnedPaste, "a", Correction { from: "Klode".into(), to: "Claude".into() }).unwrap();
    assert_eq!(count(&store, "correction_events"), 1);

    store.delete_transcription("a").unwrap();
    assert!(store.get("a").unwrap().is_none());
    assert!(!audio.join("a.wav").exists());
    assert_eq!(count(&store, "correction_events"), 0);
    assert_eq!(count(&store, "hourly_totals"), 1);
    // The learned words stay: they are the dictionary, not History.
    assert_eq!(store.active_replacements().unwrap().len(), 1);
    assert!(matches!(store.delete_transcription("a"), Err(StoreError::NotFound)));
}

#[test]
fn delete_all_history_clears_rows_and_their_audio() {
    let (d, store) = open();
    let audio = d.audio();
    for id in ["a", "b"] {
        let mut t = tx(id, utc("2026-11-02T09:00:00Z"), "hello", 1_000);
        t.audio_path = Some(wav(&audio, &format!("{id}.wav")));
        store.insert_transcription_in(&t, &utc_tz()).unwrap();
    }
    assert_eq!(store.delete_all_history().unwrap(), 2);
    assert_eq!(count(&store, "transcriptions"), 0);
    assert!(!audio.join("a.wav").exists() && !audio.join("b.wav").exists());
    assert_eq!(count(&store, "hourly_totals"), 1);
}

#[test]
fn sweep_deletes_old_rows_their_audio_and_old_orphans_but_keeps_totals() {
    let (d, store) = open();
    let audio = d.audio();
    // Files are written now, so a sweep "two hours from now" sees them as an hour+ old.
    let now = Utc::now() + Duration::hours(2);
    let retention = Duration::hours(RETENTION_HOURS);

    let mut old = tx("old", now - retention - Duration::minutes(1), "Klode old", 1_000);
    old.audio_path = Some(wav(&audio, "old.wav"));
    let mut fresh = tx("fresh", now - retention + Duration::minutes(1), "Klode fresh", 1_000);
    fresh.audio_path = Some(wav(&audio, "fresh.wav"));
    store.insert_transcription_in(&old, &utc_tz()).unwrap();
    store.insert_transcription_in(&fresh, &utc_tz()).unwrap();
    let c = || Correction { from: "Klode".into(), to: "Claude".into() };
    store.learn(Source::LearnedPaste, "old", c()).unwrap();
    store.learn(Source::LearnedPaste, "fresh", c()).unwrap();
    wav(&audio, "orphan.wav");
    wav(&audio, "ORPHAN2.WAV");
    std::fs::write(audio.join("notes.txt"), b"not audio").unwrap();
    let totals_before = count(&store, "hourly_totals");

    let report = store.sweep(now, &audio).unwrap();
    assert_eq!(
        report,
        SweepReport { rows_deleted: 1, audio_deleted: 1, orphan_audio_deleted: 2, events_deleted: 1 }
    );
    assert!(store.get("old").unwrap().is_none());
    assert!(store.get("fresh").unwrap().is_some());
    assert!(!audio.join("old.wav").exists());
    assert!(audio.join("fresh.wav").exists());
    assert!(!audio.join("orphan.wav").exists() && !audio.join("ORPHAN2.WAV").exists());
    assert!(audio.join("notes.txt").exists());
    assert_eq!(count(&store, "correction_events"), 1);
    assert_eq!(count(&store, "hourly_totals"), totals_before);
}

#[test]
fn sweep_leaves_a_brand_new_orphan_wav_alone() {
    let (d, store) = open();
    let audio = d.audio();
    wav(&audio, "recording-in-progress.wav");
    let report = store.sweep(Utc::now(), &audio).unwrap();
    assert_eq!(report.orphan_audio_deleted, 0);
    assert!(audio.join("recording-in-progress.wav").exists());
}

#[test]
fn sweep_copes_with_a_missing_audio_folder_and_missing_files() {
    let (d, store) = open();
    let now = Utc::now();
    let mut old = tx("old", now - Duration::hours(RETENTION_HOURS + 1), "x", 1_000);
    old.audio_path = Some(d.0.join("audio").join("gone.wav")); // never written
    store.insert_transcription_in(&old, &utc_tz()).unwrap();
    let report = store.sweep(now, &d.0.join("no-such-folder")).unwrap();
    assert_eq!((report.rows_deleted, report.audio_deleted), (1, 0));
}

// ---------- stats through the store ----------

#[test]
fn stats_come_from_hourly_totals_and_survive_the_sweep() {
    let (d, store) = open();
    let tz = FixedOffset::east_opt(3600).unwrap();
    store.insert_transcription_in(&tx("a", utc("2026-11-02T08:00:00Z"), "one two three four", 60_000), &tz).unwrap();
    store.insert_transcription_in(&tx("b", utc("2026-11-03T08:00:00Z"), "five six", 60_000), &tz).unwrap();
    store.sweep(utc("2026-12-01T00:00:00Z"), &d.audio()).unwrap();
    assert_eq!(count(&store, "transcriptions"), 0);
    let s = store.stats_on(NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(), 52).unwrap();
    assert_eq!((s.words_all, s.dictations_all, s.words_today, s.streak_days), (6, 2, 2, 2));
    assert_eq!(s.wpm_today, Some(2.0));
    assert_eq!(s.busiest_hour, Some(9));
}

#[test]
fn local_dates_follow_bst_so_the_streak_survives_the_clock_change() {
    // Clocks go back at 01:00 UTC on Sunday 25 Oct 2026.
    let (_d, store) = open();
    let at = |id: &str, s: &str| store.insert_transcription_in(&tx(id, utc(s), "a b", 1_000), &London).unwrap();
    at("sat", "2026-10-24T12:00:00Z"); // 13:00 BST Sat 24th
    at("sun", "2026-10-24T23:30:00Z"); // 00:30 BST Sun 25th — still the 24th in UTC
    at("mon", "2026-10-26T00:10:00Z"); // 00:10 GMT Mon 26th
    let conn = store.conn().unwrap();
    let local = |id: &str| -> (String, i64) {
        conn.query_row("SELECT local_date, local_hour FROM transcriptions WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap()
    };
    assert_eq!(local("sat"), ("2026-10-24".into(), 13));
    assert_eq!(local("sun"), ("2026-10-25".into(), 0));
    assert_eq!(local("mon"), ("2026-10-26".into(), 0));
    drop(conn);
    let s = store.stats_on(NaiveDate::from_ymd_opt(2026, 10, 26).unwrap(), 52).unwrap();
    assert_eq!(s.streak_days, 3);
    // Sanity: the test zone really is +01:00 before the change and +00:00 after.
    assert_eq!(utc("2026-10-25T00:59:00Z").with_timezone(&London).hour(), 1);
    assert_eq!(utc("2026-10-25T01:00:00Z").with_timezone(&London).hour(), 1);
}

#[test]
fn streak_counts_dictations_either_side_of_midnight_as_two_days() {
    let (_d, store) = open();
    let tz = utc_tz();
    store.insert_transcription_in(&tx("a", utc("2026-11-02T23:59:00Z"), "a", 1_000), &tz).unwrap();
    store.insert_transcription_in(&tx("b", utc("2026-11-03T00:01:00Z"), "b", 1_000), &tz).unwrap();
    let on = |d: u32| store.stats_on(NaiveDate::from_ymd_opt(2026, 11, d).unwrap(), 52).unwrap().streak_days;
    assert_eq!((on(3), on(4), on(5)), (2, 2, 0));
}

// ---------- dictionary ----------

#[test]
fn dictionary_crud() {
    let (_d, store) = open();
    let term = store.add_term("  Wren  ", Source::Manual).unwrap();
    assert_eq!((term.kind, term.to_text.as_str(), term.status, term.from_text), (EntryKind::Term, "Wren", Status::Active, None));
    // Adding the same word again (any case) gives back the same entry.
    assert_eq!(store.add_term("wren", Source::Manual).unwrap().id, term.id);
    assert!(matches!(store.add_term("   ", Source::Manual), Err(StoreError::Invalid(_))));

    let rep = store.add_replacement("tower ee", "Tauri", Source::Manual, Status::Active).unwrap();
    assert_eq!((rep.kind, rep.from_text.as_deref(), rep.to_text.as_str()), (EntryKind::Replacement, Some("tower ee"), "Tauri"));
    assert!(matches!(store.add_replacement("x", " ", Source::Manual, Status::Active), Err(StoreError::Invalid(_))));
    assert!(matches!(store.add_replacement("same", "same", Source::Manual, Status::Active), Err(StoreError::Invalid(_))));
    // Same `from` again updates the existing rule.
    let rep2 = store.add_replacement("Tower EE", "Tauri 2", Source::Manual, Status::Active).unwrap();
    assert_eq!((rep2.id, rep2.to_text.as_str()), (rep.id, "Tauri 2"));

    let list = store.dictionary_list().unwrap();
    assert_eq!(list.len(), 2);
    store.set_status(rep.id, Status::Disabled).unwrap();
    assert!(store.active_replacements().unwrap().is_empty());
    store.set_status(rep.id, Status::Active).unwrap();
    assert_eq!(store.active_replacements().unwrap(), vec![Replacement { id: rep.id, from: "Tower EE".into(), to: "Tauri 2".into() }]);
    store.delete_entry(term.id).unwrap();
    assert_eq!(store.dictionary_list().unwrap().len(), 1);
    assert!(matches!(store.delete_entry(term.id), Err(StoreError::NotFound)));
    assert!(matches!(store.set_status(term.id, Status::Active), Err(StoreError::NotFound)));

    let json = serde_json::to_value(store.dictionary_list().unwrap()).unwrap();
    assert_eq!(json[0]["fromText"], "Tower EE");
    assert_eq!(json[0]["source"], "manual");
    assert!(json[0]["createdAt"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn active_replacements_come_longest_first_and_hits_are_counted() {
    let (_d, store) = open();
    let a = store.add_replacement("Klode", "Claude", Source::Manual, Status::Active).unwrap();
    let b = store.add_replacement("clawed code", "Claude Code", Source::Manual, Status::Active).unwrap();
    store.add_replacement("Q", "queue", Source::LearnedPaste, Status::Candidate).unwrap();
    let froms: Vec<String> = store.active_replacements().unwrap().into_iter().map(|r| r.from).collect();
    assert_eq!(froms, ["clawed code", "Klode"]);
    store.record_hits(&[a.id, a.id, b.id]).unwrap();
    store.record_hits(&[]).unwrap();
    let hits = |id: i64| store.dictionary_list().unwrap().into_iter().find(|e| e.id == id).unwrap().hits;
    assert_eq!((hits(a.id), hits(b.id)), (2, 1));
}

#[test]
fn bias_terms_rank_manual_and_learned_by_hits_then_recency_then_seed_and_cap() {
    let (_d, store) = open();
    let a = store.add_term("Alpha", Source::Manual).unwrap(); // 0 hits, oldest
    let b = store.add_term("Bravo", Source::LearnedPaste).unwrap(); // 5 hits
    let c = store.add_term("Charlie", Source::Manual).unwrap(); // 5 hits, newer than Bravo
    let s = store.add_term("Seedy", Source::WillowSeed).unwrap(); // 100 hits but seed
    let x = store.add_term("Xray", Source::LearnedHistory).unwrap(); // disabled
    store.add_replacement("Klode", "Claude", Source::Manual, Status::Active).unwrap(); // not a term
    store.record_hits(&[b.id; 5]).unwrap();
    store.record_hits(&[c.id; 5]).unwrap();
    store.record_hits(&[s.id; 100]).unwrap();
    store.record_hits(&[x.id; 50]).unwrap();
    store.set_status(x.id, Status::Disabled).unwrap();
    let _ = a;
    assert_eq!(store.bias_terms(50).unwrap(), ["Charlie", "Bravo", "Alpha", "Seedy"]);
    assert_eq!(store.bias_terms(2).unwrap(), ["Charlie", "Bravo"]);
    assert!(store.bias_terms(0).unwrap().is_empty());
}

// ---------- learning and promotion ----------

fn corr(from: &str, to: &str) -> Correction {
    Correction { from: from.into(), to: to.into() }
}

fn find(store: &Store, kind: EntryKind, to: &str) -> Option<DictionaryEntry> {
    store.dictionary_list().unwrap().into_iter().find(|e| e.kind == kind && e.to_text == to)
}

#[test]
fn a_rare_from_word_becomes_an_active_replacement_and_term_at_once() {
    let (_d, store) = open();
    assert_eq!(store.learn(Source::LearnedPaste, "t1", corr("Klode", "Claude")).unwrap(), LearnDecision::AddedReplacement);
    let rep = find(&store, EntryKind::Replacement, "Claude").unwrap();
    assert_eq!((rep.from_text.as_deref(), rep.status, rep.source), (Some("Klode"), Status::Active, Source::LearnedPaste));
    let term = find(&store, EntryKind::Term, "Claude").unwrap();
    assert_eq!(term.status, Status::Active);
    // Seen again: nothing new to learn.
    assert!(matches!(store.learn(Source::LearnedPaste, "t2", corr("Klode", "Claude")).unwrap(), LearnDecision::Ignored(_)));
    assert_eq!(store.dictionary_list().unwrap().len(), 2);
    // If the term was deleted, the next sighting brings back only the term.
    store.delete_entry(term.id).unwrap();
    assert_eq!(store.learn(Source::LearnedPaste, "t3", corr("Klode", "Claude")).unwrap(), LearnDecision::AddedTerm);
    assert_eq!(count(&store, "correction_events"), 3);
}

#[test]
fn a_short_from_word_waits_for_a_second_identical_sighting() {
    let (_d, store) = open();
    assert_eq!(store.learn(Source::LearnedPaste, "t1", corr("Q", "queue")).unwrap(), LearnDecision::CandidateObserved);
    let rep = find(&store, EntryKind::Replacement, "queue").unwrap();
    assert_eq!((rep.status, rep.observations), (Status::Candidate, 1));
    assert!(store.active_replacements().unwrap().is_empty());
    // The term is active straight away.
    assert_eq!(find(&store, EntryKind::Term, "queue").unwrap().status, Status::Active);
    // A different `to` is not an identical sighting.
    assert_eq!(store.learn(Source::LearnedPaste, "t2", corr("Q", "cue")).unwrap(), LearnDecision::CandidateObserved);
    assert!(store.active_replacements().unwrap().is_empty());
    // The same pair again (any case on `from`) promotes it.
    assert_eq!(store.learn(Source::LearnedPaste, "t3", corr("q", "queue")).unwrap(), LearnDecision::AddedReplacement);
    let rep = find(&store, EntryKind::Replacement, "queue").unwrap();
    assert_eq!((rep.status, rep.observations), (Status::Active, 2));
}

#[test]
fn a_common_from_word_waits_for_a_second_sighting() {
    let (_d, store) = open();
    assert_eq!(store.learn(Source::LearnedPaste, "t1", corr("stand", "Stan")).unwrap(), LearnDecision::CandidateObserved);
    assert_eq!(store.learn(Source::LearnedPaste, "t2", corr("stand", "Stan")).unwrap(), LearnDecision::AddedReplacement);
    // A phrase of common words counts as common.
    assert_eq!(
        store.learn(Source::LearnedPaste, "t3", corr("cloud code", "Claude Code")).unwrap(),
        LearnDecision::CandidateObserved
    );
}

#[test]
fn a_disabled_rule_is_never_relearned() {
    let (_d, store) = open();
    store.learn(Source::LearnedPaste, "t1", corr("Klode", "Claude")).unwrap();
    let rep = find(&store, EntryKind::Replacement, "Claude").unwrap();
    store.set_status(rep.id, Status::Disabled).unwrap();
    assert!(matches!(store.learn(Source::LearnedPaste, "t2", corr("Klode", "Claude")).unwrap(), LearnDecision::Ignored(_)));
    assert_eq!(find(&store, EntryKind::Replacement, "Claude").unwrap().status, Status::Disabled);
}

#[test]
fn nonsense_corrections_are_ignored() {
    let (_d, store) = open();
    assert!(matches!(store.learn(Source::LearnedPaste, "t", corr("", "x")).unwrap(), LearnDecision::Ignored(_)));
    assert!(matches!(store.learn(Source::LearnedPaste, "t", corr("same", "same")).unwrap(), LearnDecision::Ignored(_)));
    assert!(store.dictionary_list().unwrap().is_empty());
}

#[test]
fn fix_word_adds_an_active_replacement_and_term() {
    let (_d, store) = open();
    let (rep, term) = store.fix_word("t1", "Klode", "Claude").unwrap();
    assert_eq!(
        (rep.kind, rep.from_text.as_deref(), rep.to_text.as_str(), rep.status, rep.source),
        (EntryKind::Replacement, Some("Klode"), "Claude", Status::Active, Source::LearnedHistory)
    );
    let term = term.unwrap();
    assert_eq!((term.kind, term.to_text.as_str(), term.status), (EntryKind::Term, "Claude", Status::Active));
    // Even a common short word is active at once: the user chose it.
    let (rep2, term2) = store.fix_word("t1", "Q", "Claude").unwrap();
    assert_eq!(rep2.status, Status::Active);
    assert!(term2.is_none(), "the term already exists");
    // Fixing the same heard word again rewrites its rule rather than adding a second one.
    let (rep3, _) = store.fix_word("t2", "klode", "Clode").unwrap();
    assert_eq!((rep3.id, rep3.to_text.as_str()), (rep.id, "Clode"));
    assert!(matches!(store.fix_word("t1", "a", " "), Err(StoreError::Invalid(_))));
    assert!(matches!(store.fix_word("t1", "same", "same"), Err(StoreError::Invalid(_))));
}

#[test]
fn fix_word_reenables_a_disabled_term() {
    let (_d, store) = open();
    let t = store.add_term("Claude", Source::Manual).unwrap();
    store.set_status(t.id, Status::Disabled).unwrap();
    let (_, term) = store.fix_word("t1", "Klode", "Claude").unwrap();
    assert_eq!(term.map(|e| (e.id, e.status)), Some((t.id, Status::Active)));
}
