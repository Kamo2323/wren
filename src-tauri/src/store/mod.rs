//! The SQLite store (spec §9): History, dictionary entries, correction events and the hourly
//! totals behind the stats. Owned by Rust; the UI reaches it only through commands.

pub mod schema;
pub mod stats;

use crate::constants::{
    DB_BUSY_TIMEOUT_MS, LEARN_PROMOTE_SIGHTINGS, LEARN_SHORT_FROM_MAX_CHARS, ORPHAN_AUDIO_MIN_AGE_MS,
    RETENTION_HOURS,
};
use crate::dictionary::learn::{Correction, LearnDecision};
use crate::dictionary::{is_common_word, replace_whole_word, DictionaryEntry, EntryKind, Replacement, Source, Status};
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Timelike, Utc};
use parking_lot::{Mutex, MutexGuard};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
pub use stats::{DayPoint, Stats};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PasteOutcome {
    Pasted,
    NotPasted,
    Copied,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Mode {
    Hold,
    HandsFree,
}

/// Everything saved about one dictation (spec §9 `transcriptions`).
#[derive(Debug, Clone)]
pub struct NewTranscription {
    pub id: String,
    pub created_at_utc: DateTime<Utc>,
    pub mode: Mode,
    pub audio_path: Option<PathBuf>,
    pub duration_ms: u64,
    pub speech_ms: u64,
    pub raw_text: String,
    pub tidied_text: String,
    pub final_text: String,
    pub model: String,
    pub device: String,
    pub timings_json: String,
    pub target_process: String,
    pub paste_outcome: PasteOutcome,
    pub applied_rule_ids: Vec<i64>,
}

/// A History card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionRow {
    pub id: String,
    /// RFC 3339 UTC.
    pub created_at_utc: String,
    pub mode: Mode,
    pub duration_ms: u64,
    pub final_text: String,
    pub raw_text: String,
    pub words: u64,
    pub target_process: String,
    pub paste_outcome: PasteOutcome,
}

/// What one retention sweep removed (counts only — safe to log).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SweepReport {
    pub rows_deleted: usize,
    pub audio_deleted: usize,
    pub orphan_audio_deleted: usize,
    pub events_deleted: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("History and the dictionary are unavailable: {0}")]
    Unavailable(String),
    #[error("The database was made by a newer version of Wren (schema {found}; this version knows {known}).")]
    TooNew { found: i64, known: i64 },
    #[error("Database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("File error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("That item no longer exists.")]
    NotFound,
}

pub type StoreResult<T> = Result<T, StoreError>;

/// The database handle. If the file could not be opened at startup the store is *unavailable*:
/// the app keeps running and every call returns `StoreError::Unavailable` with the reason.
pub struct Store {
    conn: Option<Mutex<Connection>>,
    unavailable: Option<String>,
}

/// RFC 3339 UTC with milliseconds — sorts correctly as text.
fn ts(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn now_ts() -> String {
    ts(Utc::now())
}

/// Whitespace-separated words (spec §9).
fn word_count(text: &str) -> u64 {
    text.split_whitespace().count() as u64
}

fn bad_value(col: usize, what: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        col,
        rusqlite::types::Type::Text,
        format!("unexpected {what} '{value}'").into(),
    )
}

impl Mode {
    fn as_db(self) -> &'static str {
        match self {
            Mode::Hold => "hold",
            Mode::HandsFree => "handsfree",
        }
    }
    fn from_db(s: &str) -> Option<Self> {
        match s {
            "hold" => Some(Mode::Hold),
            "handsfree" => Some(Mode::HandsFree),
            _ => None,
        }
    }
}

impl PasteOutcome {
    fn as_db(self) -> &'static str {
        match self {
            PasteOutcome::Pasted => "pasted",
            PasteOutcome::NotPasted => "not_pasted",
            PasteOutcome::Copied => "copied",
            PasteOutcome::Failed => "failed",
        }
    }
    fn from_db(s: &str) -> Option<Self> {
        match s {
            "pasted" => Some(PasteOutcome::Pasted),
            "not_pasted" => Some(PasteOutcome::NotPasted),
            "copied" => Some(PasteOutcome::Copied),
            "failed" => Some(PasteOutcome::Failed),
            _ => None,
        }
    }
}

const ROW_COLS: &str =
    "id, created_at_utc, mode, duration_ms, final_text, raw_text, words, target_process, paste_outcome";

fn row_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptionRow> {
    let mode: String = r.get(2)?;
    let outcome: String = r.get(8)?;
    Ok(TranscriptionRow {
        id: r.get(0)?,
        created_at_utc: r.get(1)?,
        mode: Mode::from_db(&mode).ok_or_else(|| bad_value(2, "mode", &mode))?,
        duration_ms: r.get::<_, i64>(3)?.max(0) as u64,
        final_text: r.get(4)?,
        raw_text: r.get(5)?,
        words: r.get::<_, i64>(6)?.max(0) as u64,
        target_process: r.get(7)?,
        paste_outcome: PasteOutcome::from_db(&outcome).ok_or_else(|| bad_value(8, "paste outcome", &outcome))?,
    })
}

const ENTRY_COLS: &str = "id, kind, from_text, to_text, source, status, observations, hits, created_at";

fn entry_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<DictionaryEntry> {
    let kind: String = r.get(1)?;
    let source: String = r.get(4)?;
    let status: String = r.get(5)?;
    Ok(DictionaryEntry {
        id: r.get(0)?,
        kind: EntryKind::from_db(&kind).ok_or_else(|| bad_value(1, "kind", &kind))?,
        from_text: r.get(2)?,
        to_text: r.get(3)?,
        source: Source::from_db(&source).ok_or_else(|| bad_value(4, "source", &source))?,
        status: Status::from_db(&status).ok_or_else(|| bad_value(5, "status", &status))?,
        observations: r.get::<_, i64>(6)?.max(0) as u32,
        hits: r.get::<_, i64>(7)?.max(0) as u32,
        created_at: r.get(8)?,
    })
}

fn entry_by_id(conn: &Connection, id: i64) -> StoreResult<DictionaryEntry> {
    conn.query_row(&format!("SELECT {ENTRY_COLS} FROM dictionary_entries WHERE id = ?1"), [id], entry_from)
        .optional()?
        .ok_or(StoreError::NotFound)
}

fn find_term(conn: &Connection, to: &str) -> StoreResult<Option<DictionaryEntry>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ENTRY_COLS} FROM dictionary_entries
                 WHERE kind = 'term' AND to_text = ?1 COLLATE NOCASE ORDER BY id LIMIT 1"
            ),
            [to],
            entry_from,
        )
        .optional()?)
}

/// The replacement for exactly this pair (`from` ignoring case, `to` exact).
fn find_pair(conn: &Connection, from: &str, to: &str) -> StoreResult<Option<DictionaryEntry>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ENTRY_COLS} FROM dictionary_entries
                 WHERE kind = 'replacement' AND from_text = ?1 COLLATE NOCASE AND to_text = ?2
                 ORDER BY id LIMIT 1"
            ),
            params![from, to],
            entry_from,
        )
        .optional()?)
}

/// Any replacement for this `from` (ignoring case), the active one first.
fn find_by_from(conn: &Connection, from: &str) -> StoreResult<Option<DictionaryEntry>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ENTRY_COLS} FROM dictionary_entries
                 WHERE kind = 'replacement' AND from_text = ?1 COLLATE NOCASE
                 ORDER BY CASE status WHEN 'active' THEN 0 WHEN 'candidate' THEN 1 ELSE 2 END, id
                 LIMIT 1"
            ),
            [from],
            entry_from,
        )
        .optional()?)
}

#[allow(clippy::too_many_arguments)]
fn insert_entry(
    conn: &Connection,
    kind: EntryKind,
    from: Option<&str>,
    to: &str,
    source: Source,
    status: Status,
    origin: Option<&str>,
    batch: Option<&str>,
) -> StoreResult<i64> {
    let now = now_ts();
    conn.execute(
        "INSERT INTO dictionary_entries
           (kind, from_text, to_text, source, status, observations, hits,
            origin_transcription_id, batch_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, ?6, ?7, ?8, ?8)",
        params![kind.as_db(), from, to, source.as_db(), status.as_db(), origin, batch, now],
    )?;
    Ok(conn.last_insert_rowid())
}

fn update_status(conn: &Connection, id: i64, status: Status) -> StoreResult<()> {
    let n = conn.execute(
        "UPDATE dictionary_entries SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status.as_db(), now_ts()],
    )?;
    if n == 0 { Err(StoreError::NotFound) } else { Ok(()) }
}

/// Once a rule for `from` is active, other live rules for the same `from` are superseded and
/// removed. Disabled ones stay: they record that the user said no.
fn drop_superseded(conn: &Connection, keep_id: i64, from: &str) -> StoreResult<()> {
    conn.execute(
        "DELETE FROM dictionary_entries
         WHERE kind = 'replacement' AND from_text = ?1 COLLATE NOCASE AND id <> ?2 AND status <> 'disabled'",
        params![from, keep_id],
    )?;
    Ok(())
}

/// Make `from → to` the active rule for `from`, reusing an existing row where there is one.
fn activate_replacement(
    conn: &Connection,
    from: &str,
    to: &str,
    source: Source,
    origin: Option<&str>,
    batch: Option<&str>,
) -> StoreResult<i64> {
    let existing = match find_pair(conn, from, to)? {
        Some(e) => Some(e),
        None => find_by_from(conn, from)?,
    };
    let id = match existing {
        Some(e) => {
            conn.execute(
                "UPDATE dictionary_entries
                 SET from_text = ?2, to_text = ?3, source = ?4, status = 'active',
                     origin_transcription_id = COALESCE(?5, origin_transcription_id),
                     batch_id = COALESCE(?6, batch_id), updated_at = ?7
                 WHERE id = ?1",
                params![e.id, from, to, source.as_db(), origin, batch, now_ts()],
            )?;
            e.id
        }
        None => insert_entry(conn, EntryKind::Replacement, Some(from), to, source, Status::Active, origin, batch)?,
    };
    drop_superseded(conn, id, from)?;
    Ok(id)
}

/// Ensure an active term for `to`. Returns the entry when it was added or re-enabled.
/// A term the user disabled stays disabled unless `reenable` is set (an explicit fix).
fn ensure_term(
    conn: &Connection,
    to: &str,
    source: Source,
    origin: Option<&str>,
    batch: Option<&str>,
    reenable: bool,
) -> StoreResult<Option<DictionaryEntry>> {
    match find_term(conn, to)? {
        None => {
            let id = insert_entry(conn, EntryKind::Term, None, to, source, Status::Active, origin, batch)?;
            Ok(Some(entry_by_id(conn, id)?))
        }
        Some(e) if e.status == Status::Active => Ok(None),
        Some(e) if e.status == Status::Disabled && !reenable => Ok(None),
        Some(e) => {
            update_status(conn, e.id, Status::Active)?;
            Ok(Some(entry_by_id(conn, e.id)?))
        }
    }
}

fn record_event(conn: &Connection, transcription_id: &str, from: &str, to: &str, source: Source, decision: &str) -> StoreResult<()> {
    conn.execute(
        "INSERT INTO correction_events (transcription_id, from_text, to_text, source, decision, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![transcription_id, from, to, source.as_db(), decision, now_ts()],
    )?;
    Ok(())
}

/// Delete one audio file. A file that is already gone is fine; other failures are logged
/// (without any path text beyond the error kind) and the caller carries on.
fn remove_audio(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            tracing::warn!(error_kind = ?e.kind(), "could not delete an audio file");
            false
        }
    }
}

/// Fold the write-ahead log back into the database and truncate it, so text that was just
/// deleted does not linger in `wren.db-wal` (`secure_delete` already zeroes the main file).
fn checkpoint_truncate(conn: &Connection) {
    let res = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get::<_, i64>(0));
    match res {
        Ok(0) => {}
        Ok(_) => tracing::warn!("WAL checkpoint was blocked; deleted text stays in the log until the next one"),
        Err(e) => tracing::warn!(error = %e, "WAL checkpoint failed"),
    }
}

fn trimmed_pair<'a>(from: &'a str, to: &'a str) -> StoreResult<(&'a str, &'a str)> {
    let (from, to) = (from.trim(), to.trim());
    if from.is_empty() || to.is_empty() {
        return Err(StoreError::Invalid("Type both the word Wren wrote and the word it should write.".into()));
    }
    if from == to {
        return Err(StoreError::Invalid("The replacement is the same as the original.".into()));
    }
    Ok((from, to))
}

impl Store {
    /// Open (creating if needed) the database at `path`: WAL, `secure_delete=ON`, migrated.
    pub fn open(path: &Path) -> StoreResult<Store> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(DB_BUSY_TIMEOUT_MS))?;
        let mode = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get::<_, String>(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            tracing::warn!(journal_mode = %mode, "the database did not switch to WAL; carrying on");
        }
        conn.pragma_update_and_check(None, "secure_delete", true, |r| r.get::<_, i64>(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        if let schema::Migrated::TooNew { found, known } = schema::migrate(&mut conn)? {
            return Err(StoreError::TooNew { found, known });
        }
        Ok(Store { conn: Some(Mutex::new(conn)), unavailable: None })
    }

    /// A store that could not be opened; every call fails with `reason`.
    pub fn unavailable(reason: impl Into<String>) -> Store {
        Store { conn: None, unavailable: Some(reason.into()) }
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    fn conn(&self) -> StoreResult<MutexGuard<'_, Connection>> {
        match &self.conn {
            Some(c) => Ok(c.lock()),
            None => Err(StoreError::Unavailable(self.unavailable.clone().unwrap_or_default())),
        }
    }

    // ---- transcriptions ----

    /// Save a dictation and add it to `hourly_totals` in the same transaction (local time =
    /// this PC's zone). Dictionary hit counts are separate: call `record_hits`.
    pub fn insert_transcription(&self, t: &NewTranscription) -> StoreResult<()> {
        self.insert_transcription_in(t, &Local)
    }

    /// `insert_transcription` with an explicit zone (tests use fixed offsets and a BST zone).
    pub(crate) fn insert_transcription_in<Tz: TimeZone>(&self, t: &NewTranscription, tz: &Tz) -> StoreResult<()> {
        let local = t.created_at_utc.with_timezone(tz);
        let local_date = local.date_naive().format("%Y-%m-%d").to_string();
        let local_hour = local.hour() as i64;
        let words = word_count(&t.final_text) as i64;
        let rule_ids = serde_json::to_string(&t.applied_rule_ids).unwrap_or_else(|_| "[]".into());
        let audio = t.audio_path.as_ref().map(|p| p.to_string_lossy().into_owned());

        let mut conn = self.conn()?;
        let txn = conn.transaction()?;
        txn.execute(
            "INSERT INTO transcriptions
               (id, created_at_utc, local_date, local_hour, mode, audio_path, duration_ms, speech_ms,
                raw_text, tidied_text, final_text, words, model, device, timings_json, target_process,
                paste_outcome, applied_rule_ids)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                t.id,
                ts(t.created_at_utc),
                local_date,
                local_hour,
                t.mode.as_db(),
                audio,
                t.duration_ms as i64,
                t.speech_ms as i64,
                t.raw_text,
                t.tidied_text,
                t.final_text,
                words,
                t.model,
                t.device,
                t.timings_json,
                t.target_process,
                t.paste_outcome.as_db(),
                rule_ids,
            ],
        )?;
        txn.execute(
            "INSERT INTO hourly_totals (local_date, hour, dictations, words, speaking_ms)
             VALUES (?1, ?2, 1, ?3, ?4)
             ON CONFLICT (local_date, hour) DO UPDATE SET
               dictations = dictations + 1,
               words = words + excluded.words,
               speaking_ms = speaking_ms + excluded.speaking_ms",
            params![local_date, local_hour, words, t.duration_ms as i64],
        )?;
        txn.commit()?;
        Ok(())
    }

    pub fn set_paste_outcome(&self, id: &str, outcome: PasteOutcome) -> StoreResult<()> {
        let n = self.conn()?.execute(
            "UPDATE transcriptions SET paste_outcome = ?2 WHERE id = ?1",
            params![id, outcome.as_db()],
        )?;
        if n == 0 { Err(StoreError::NotFound) } else { Ok(()) }
    }

    /// Newest first.
    pub fn recent(&self, limit: u32) -> StoreResult<Vec<TranscriptionRow>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(&format!(
            "SELECT {ROW_COLS} FROM transcriptions ORDER BY created_at_utc DESC, rowid DESC LIMIT ?1"
        ))?;
        let rows = st.query_map([limit as i64], row_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// `LIKE` over `final_text` (case-insensitive for ASCII; `%` and `_` match literally).
    /// A blank query returns everything, newest first.
    pub fn search(&self, query: &str) -> StoreResult<Vec<TranscriptionRow>> {
        let q = query.trim();
        let conn = self.conn()?;
        let rows = if q.is_empty() {
            let mut st = conn.prepare(&format!(
                "SELECT {ROW_COLS} FROM transcriptions ORDER BY created_at_utc DESC, rowid DESC"
            ))?;
            st.query_map([], row_from)?.collect::<rusqlite::Result<Vec<_>>>()?
        } else {
            let escaped = q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            let mut st = conn.prepare(&format!(
                "SELECT {ROW_COLS} FROM transcriptions WHERE final_text LIKE ?1 ESCAPE '\\'
                 ORDER BY created_at_utc DESC, rowid DESC"
            ))?;
            st.query_map([format!("%{escaped}%")], row_from)?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        Ok(rows)
    }

    pub fn get(&self, id: &str) -> StoreResult<Option<TranscriptionRow>> {
        Ok(self
            .conn()?
            .query_row(&format!("SELECT {ROW_COLS} FROM transcriptions WHERE id = ?1"), [id], row_from)
            .optional()?)
    }

    /// Delete one dictation, its audio and its correction events. Hourly totals stay.
    pub fn delete_transcription(&self, id: &str) -> StoreResult<()> {
        let mut conn = self.conn()?;
        let audio: Option<String> = conn
            .query_row("SELECT audio_path FROM transcriptions WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if let Some(p) = audio {
            remove_audio(Path::new(&p));
        }
        let txn = conn.transaction()?;
        txn.execute("DELETE FROM correction_events WHERE transcription_id = ?1", [id])?;
        txn.execute("DELETE FROM transcriptions WHERE id = ?1", [id])?;
        txn.commit()?;
        checkpoint_truncate(&conn);
        Ok(())
    }

    /// Delete every dictation, its audio and every correction event. Hourly totals (counts only)
    /// and the dictionary stay. Returns how many dictations were deleted.
    pub fn delete_all_history(&self) -> StoreResult<usize> {
        let mut conn = self.conn()?;
        let audio: Vec<String> = {
            let mut st = conn.prepare("SELECT audio_path FROM transcriptions WHERE audio_path IS NOT NULL")?;
            st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for p in &audio {
            remove_audio(Path::new(p));
        }
        let txn = conn.transaction()?;
        txn.execute("DELETE FROM correction_events", [])?;
        let n = txn.execute("DELETE FROM transcriptions", [])?;
        txn.commit()?;
        checkpoint_truncate(&conn);
        Ok(n)
    }

    /// Retention (spec §9): delete the audio, then the rows, of dictations older than
    /// `RETENTION_HOURS`; delete correction events whose dictation is gone; delete WAVs in
    /// `audio_dir` that no row refers to (once they are `ORPHAN_AUDIO_MIN_AGE_MS` old).
    pub fn sweep(&self, now: DateTime<Utc>, audio_dir: &Path) -> StoreResult<SweepReport> {
        let mut report = SweepReport::default();
        let cutoff = ts(now - Duration::hours(RETENTION_HOURS));
        let mut conn = self.conn()?;

        let old_audio: Vec<String> = {
            let mut st = conn.prepare(
                "SELECT audio_path FROM transcriptions WHERE created_at_utc < ?1 AND audio_path IS NOT NULL",
            )?;
            st.query_map([&cutoff], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for p in &old_audio {
            if remove_audio(Path::new(p)) {
                report.audio_deleted += 1;
            }
        }
        let txn = conn.transaction()?;
        report.rows_deleted = txn.execute("DELETE FROM transcriptions WHERE created_at_utc < ?1", [&cutoff])?;
        report.events_deleted = txn.execute(
            "DELETE FROM correction_events WHERE transcription_id NOT IN (SELECT id FROM transcriptions)",
            [],
        )?;
        txn.commit()?;
        if report.rows_deleted > 0 || report.events_deleted > 0 {
            checkpoint_truncate(&conn);
        }

        let referenced: HashSet<String> = {
            let mut st = conn.prepare("SELECT audio_path FROM transcriptions WHERE audio_path IS NOT NULL")?;
            st.query_map([], |r| r.get::<_, String>(0))?
                .filter_map(Result::ok)
                .filter_map(|p| Path::new(&p).file_name().map(|n| n.to_string_lossy().to_lowercase()))
                .collect()
        };
        drop(conn);

        let min_age = Duration::milliseconds(ORPHAN_AUDIO_MIN_AGE_MS as i64);
        if let Ok(entries) = std::fs::read_dir(audio_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_wav = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav"));
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else { continue };
                if !is_wav || !path.is_file() || referenced.contains(&name) {
                    continue;
                }
                let old_enough = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|m| now - DateTime::<Utc>::from(m) >= min_age)
                    .unwrap_or(false);
                if old_enough && remove_audio(&path) {
                    report.orphan_audio_deleted += 1;
                }
            }
        }
        Ok(report)
    }

    // ---- stats ----

    /// Stats for the local day containing `now_local`.
    pub fn stats(&self, now_local: DateTime<Local>, typing_wpm: u32) -> StoreResult<Stats> {
        self.stats_on(now_local.date_naive(), typing_wpm)
    }

    /// Stats with `today` given as a local calendar date.
    pub fn stats_on(&self, today: NaiveDate, typing_wpm: u32) -> StoreResult<Stats> {
        let conn = self.conn()?;
        let mut st = conn.prepare("SELECT local_date, hour, dictations, words, speaking_ms FROM hourly_totals")?;
        let rows: Vec<stats::HourRow> = st
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(date, hour, dictations, words, ms)| {
                Some(stats::HourRow {
                    date: NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?,
                    hour: hour.clamp(0, 23) as u8,
                    dictations: dictations.max(0) as u64,
                    words: words.max(0) as u64,
                    speaking_ms: ms.max(0) as u64,
                })
            })
            .collect();
        Ok(stats::compute(&rows, today, typing_wpm))
    }

    // ---- dictionary ----

    /// Every entry, newest first.
    pub fn dictionary_list(&self) -> StoreResult<Vec<DictionaryEntry>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(&format!(
            "SELECT {ENTRY_COLS} FROM dictionary_entries ORDER BY created_at DESC, id DESC"
        ))?;
        let rows = st.query_map([], entry_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Add an active term. Adding a word that is already there (any case) returns that entry,
    /// switched back on if it was off.
    pub fn add_term(&self, word: &str, source: Source) -> StoreResult<DictionaryEntry> {
        let word = word.trim();
        if word.is_empty() {
            return Err(StoreError::Invalid("Type a word first.".into()));
        }
        let conn = self.conn()?;
        match find_term(&conn, word)? {
            Some(e) => {
                if e.status != Status::Active {
                    update_status(&conn, e.id, Status::Active)?;
                }
                entry_by_id(&conn, e.id)
            }
            None => {
                let id = insert_entry(&conn, EntryKind::Term, None, word, source, Status::Active, None, None)?;
                entry_by_id(&conn, id)
            }
        }
    }

    /// Add a replacement. An active one replaces any live rule for the same `from` (ignoring case).
    pub fn add_replacement(&self, from: &str, to: &str, source: Source, status: Status) -> StoreResult<DictionaryEntry> {
        let (from, to) = trimmed_pair(from, to)?;
        let mut conn = self.conn()?;
        let txn = conn.transaction()?;
        let id = if status == Status::Active {
            activate_replacement(&txn, from, to, source, None, None)?
        } else if let Some(e) = find_pair(&txn, from, to)? {
            update_status(&txn, e.id, status)?;
            e.id
        } else {
            insert_entry(&txn, EntryKind::Replacement, Some(from), to, source, status, None, None)?
        };
        let entry = entry_by_id(&txn, id)?;
        txn.commit()?;
        Ok(entry)
    }

    /// Change an entry's status. Switching a replacement on removes any other live rule for
    /// the same `from`, so one heard word never has two active rules.
    pub fn set_status(&self, id: i64, status: Status) -> StoreResult<()> {
        let mut conn = self.conn()?;
        let txn = conn.transaction()?;
        update_status(&txn, id, status)?;
        if status == Status::Active {
            let entry = entry_by_id(&txn, id)?;
            if let (EntryKind::Replacement, Some(from)) = (entry.kind, entry.from_text.as_deref()) {
                drop_superseded(&txn, id, from)?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    pub fn delete_entry(&self, id: i64) -> StoreResult<()> {
        let n = self.conn()?.execute("DELETE FROM dictionary_entries WHERE id = ?1", [id])?;
        if n == 0 { Err(StoreError::NotFound) } else { Ok(()) }
    }

    /// Add one hit per id (an id listed twice counts twice). Unknown ids are skipped.
    pub fn record_hits(&self, ids: &[i64]) -> StoreResult<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn()?;
        let txn = conn.transaction()?;
        {
            let mut st = txn.prepare("UPDATE dictionary_entries SET hits = hits + 1 WHERE id = ?1")?;
            for id in ids {
                st.execute([id])?;
            }
        }
        txn.commit()?;
        Ok(())
    }

    /// Terms to bias the model (spec §8): active manual and learned terms by hits then recency,
    /// then seed terms, at most `max`.
    pub fn bias_terms(&self, max: usize) -> StoreResult<Vec<String>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(
            "SELECT to_text FROM dictionary_entries
             WHERE kind = 'term' AND status = 'active'
             ORDER BY CASE source WHEN 'willow_seed' THEN 1 ELSE 0 END, hits DESC, created_at DESC, id DESC
             LIMIT ?1",
        )?;
        let rows = st.query_map([max as i64], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Active replacements, longest `from` first (the pipeline applies them in this order).
    pub fn active_replacements(&self) -> StoreResult<Vec<Replacement>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(
            "SELECT id, from_text, to_text FROM dictionary_entries
             WHERE kind = 'replacement' AND status = 'active'
             ORDER BY length(from_text) DESC, id ASC",
        )?;
        let rows = st
            .query_map([], |r| Ok(Replacement { id: r.get(0)?, from: r.get(1)?, to: r.get(2)? }))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Learn from one auto-detected correction (spec §8 promotion). `to` becomes an active term
    /// at once. The `from → to` replacement is active at once when `from` is not a common
    /// English word and is longer than `LEARN_SHORT_FROM_MAX_CHARS`; otherwise it waits as a
    /// candidate until `LEARN_PROMOTE_SIGHTINGS` identical sightings. A rule the user disabled is
    /// never re-learned.
    pub fn learn(&self, source: Source, transcription_id: &str, c: Correction) -> StoreResult<LearnDecision> {
        let (from, to) = (c.from.trim(), c.to.trim());
        if from.is_empty() || to.is_empty() {
            return Ok(LearnDecision::Ignored("empty correction".into()));
        }
        if from == to {
            return Ok(LearnDecision::Ignored("nothing changed".into()));
        }
        let batch = uuid::Uuid::new_v4().to_string();
        let origin = Some(transcription_id);
        let mut conn = self.conn()?;
        let txn = conn.transaction()?;

        enum Rule {
            Disabled,
            AlreadyActive,
            NowActive,
            Waiting,
        }
        let rule = match find_pair(&txn, from, to)? {
            Some(e) if e.status == Status::Disabled => Rule::Disabled,
            Some(e) => {
                txn.execute(
                    "UPDATE dictionary_entries SET observations = observations + 1, updated_at = ?2 WHERE id = ?1",
                    params![e.id, now_ts()],
                )?;
                if e.status == Status::Active {
                    Rule::AlreadyActive
                } else if e.observations + 1 >= LEARN_PROMOTE_SIGHTINGS {
                    update_status(&txn, e.id, Status::Active)?;
                    drop_superseded(&txn, e.id, from)?;
                    Rule::NowActive
                } else {
                    Rule::Waiting
                }
            }
            None => {
                let at_once = !is_common_word(from) && from.chars().count() > LEARN_SHORT_FROM_MAX_CHARS;
                let status = if at_once { Status::Active } else { Status::Candidate };
                let id = insert_entry(&txn, EntryKind::Replacement, Some(from), to, source, status, origin, Some(&batch))?;
                if at_once {
                    drop_superseded(&txn, id, from)?;
                    Rule::NowActive
                } else {
                    Rule::Waiting
                }
            }
        };

        let decision = if let Rule::Disabled = rule {
            LearnDecision::Ignored("the user turned this rule off".into())
        } else {
            let term_added = ensure_term(&txn, to, source, origin, Some(&batch), false)?.is_some();
            match rule {
                Rule::NowActive => LearnDecision::AddedReplacement,
                Rule::Waiting => LearnDecision::CandidateObserved,
                _ if term_added => LearnDecision::AddedTerm,
                _ => LearnDecision::Ignored("already known".into()),
            }
        };
        let label = match &decision {
            LearnDecision::AddedTerm => "added_term".to_string(),
            LearnDecision::AddedReplacement => "added_replacement".to_string(),
            LearnDecision::CandidateObserved => "candidate".to_string(),
            LearnDecision::Ignored(why) => format!("ignored: {why}"),
        };
        record_event(&txn, transcription_id, from, to, source, &label)?;
        txn.commit()?;
        Ok(decision)
    }

    /// History "fix a word" (spec §8): an active replacement `from → to` and an active term `to`,
    /// whatever the word, and the transcription's `final_text` rewritten the same way (whole word,
    /// case-aware) so the fix survives a reload. `words` follows the new text; `hourly_totals` stay as
    /// they were (stats count what was dictated). Returns the replacement and the term if it was added
    /// or switched back on.
    pub fn fix_word(&self, transcription_id: &str, from: &str, to: &str) -> StoreResult<(DictionaryEntry, Option<DictionaryEntry>)> {
        let (from, to) = trimmed_pair(from, to)?;
        let batch = uuid::Uuid::new_v4().to_string();
        let origin = Some(transcription_id);
        let mut conn = self.conn()?;
        let txn = conn.transaction()?;
        let id = activate_replacement(&txn, from, to, Source::LearnedHistory, origin, Some(&batch))?;
        let term = ensure_term(&txn, to, Source::LearnedHistory, origin, Some(&batch), true)?;
        record_event(&txn, transcription_id, from, to, Source::LearnedHistory, "fixed")?;
        let text: Option<String> = txn
            .query_row("SELECT final_text FROM transcriptions WHERE id = ?1", params![transcription_id], |r| r.get(0))
            .optional()?;
        if let Some(text) = text {
            let fixed = replace_whole_word(&text, from, to);
            if fixed != text {
                txn.execute(
                    "UPDATE transcriptions SET final_text = ?1, words = ?2 WHERE id = ?3",
                    params![fixed, word_count(&fixed) as i64, transcription_id],
                )?;
            }
        }
        let rep = entry_by_id(&txn, id)?;
        txn.commit()?;
        Ok((rep, term))
    }
}

#[cfg(test)]
mod tests;
