//! Meeting storage (plan Task 1; spec §5.5). Lives under `store` so it shares the connection and helpers.
//! Meeting audio is kept for `MEETING_AUDIO_RETENTION_HOURS` after the meeting ends; transcript, the
//! user's notes and enhanced notes stay until the meeting is deleted.

use super::{remove_audio, ts, Store, StoreError, StoreResult};
use crate::constants::{MEETING_AUDIO_RETENTION_HOURS, ORPHAN_AUDIO_MIN_AGE_MS};
use crate::meetings::{CaptureMode, Length, MeetingRow, MeetingStatus, NewSegment, NoteRow, SegmentRow, Source};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, OptionalExtension, Row};
use std::path::Path;

const PREVIEW_MAX_CHARS: usize = 140;

const MEETING_COLS: &str = "id, title, title_edited, started_at_utc, ended_at_utc, duration_ms, status, capture_mode, \
     my_notes, participants_json, teams_self_name, audio_deleted_at";

fn bad(col: usize, what: &str, v: &str) -> rusqlite::Error {
    super::bad_value(col, what, v)
}

fn meeting_from_row(r: &Row<'_>) -> rusqlite::Result<MeetingRow> {
    let status: String = r.get(6)?;
    let capture: String = r.get(7)?;
    let participants: String = r.get(9)?;
    let audio_deleted: Option<String> = r.get(11)?;
    Ok(MeetingRow {
        id: r.get(0)?,
        title: r.get(1)?,
        title_edited: r.get::<_, i64>(2)? != 0,
        started_at_utc: r.get(3)?,
        ended_at_utc: r.get(4)?,
        duration_ms: r.get::<_, i64>(5)?.max(0) as u64,
        status: MeetingStatus::from_db(&status).ok_or_else(|| bad(6, "status", &status))?,
        capture_mode: CaptureMode::from_db(&capture).ok_or_else(|| bad(7, "capture mode", &capture))?,
        my_notes: r.get(8)?,
        participants: serde_json::from_str(&participants).unwrap_or_default(),
        teams_self_name: r.get(10)?,
        audio_available: audio_deleted.is_none(),
        preview: None,
    })
}

fn segment_from_row(r: &Row<'_>) -> rusqlite::Result<SegmentRow> {
    let source: String = r.get(2)?;
    Ok(SegmentRow {
        id: r.get(0)?,
        meeting_id: r.get(1)?,
        source: Source::from_db(&source).ok_or_else(|| bad(2, "source", &source))?,
        speaker: r.get(3)?,
        confidence: r.get::<_, f64>(4)? as f32,
        renamed: r.get::<_, i64>(5)? != 0,
        muted: r.get::<_, i64>(6)? != 0,
        start_ms: r.get::<_, i64>(7)?.max(0) as u64,
        end_ms: r.get::<_, i64>(8)?.max(0) as u64,
        text: r.get(9)?,
    })
}

fn note_from_row(r: &Row<'_>) -> rusqlite::Result<NoteRow> {
    let length: String = r.get(1)?;
    Ok(NoteRow {
        meeting_id: r.get(0)?,
        length: Length::from_db(&length).ok_or_else(|| bad(1, "length", &length))?,
        markdown: r.get(2)?,
        model: r.get(3)?,
        input_tokens: r.get::<_, i64>(4)?.max(0) as u64,
        output_tokens: r.get::<_, i64>(5)?.max(0) as u64,
        cache_read_tokens: r.get::<_, i64>(6)?.max(0) as u64,
        cache_write_tokens: r.get::<_, i64>(7)?.max(0) as u64,
        cost_usd: r.get(8)?,
        created_at_utc: r.get(9)?,
        stale: r.get::<_, i64>(10)? != 0,
    })
}

/// First real line of a markdown note: skips headings and blank lines, strips list markers.
pub(crate) fn first_summary_line(md: &str) -> Option<String> {
    md.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let l = l.trim_start_matches(['-', '*', '+']).trim_start();
            l.trim_start_matches("[ ]").trim_start_matches("[x]").trim().replace("**", "")
        })
        .find(|l| !l.is_empty())
}

fn clip(s: &str) -> String {
    if s.chars().count() <= PREVIEW_MAX_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(PREVIEW_MAX_CHARS - 1).collect();
    out.push('…');
    out
}

impl Store {
    /// Start a new meeting row in `recording` state.
    pub fn meeting_create(&self, title: &str, capture: CaptureMode) -> StoreResult<MeetingRow> {
        let id = uuid::Uuid::new_v4().to_string();
        // Millisecond timestamps; ids break ties so two meetings started in the same ms still order.
        let started = ts(Utc::now());
        {
            let conn = self.conn()?;
            conn.execute(
                "INSERT INTO meetings (id, title, started_at_utc, status, capture_mode) VALUES (?1, ?2, ?3, 'recording', ?4)",
                params![id, title, started, capture.as_db()],
            )?;
        }
        self.meeting_get(&id)?.ok_or(StoreError::NotFound)
    }

    pub fn meeting_set_status(
        &self,
        id: &str,
        status: MeetingStatus,
        ended_at: Option<DateTime<Utc>>,
        duration_ms: u64,
    ) -> StoreResult<()> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE meetings SET status = ?2, ended_at_utc = COALESCE(?3, ended_at_utc), duration_ms = ?4 WHERE id = ?1",
            params![id, status.as_db(), ended_at.map(ts), duration_ms as i64],
        )?;
        if n == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub fn meeting_set_capture_mode(&self, id: &str, mode: CaptureMode) -> StoreResult<()> {
        let conn = self.conn()?;
        conn.execute("UPDATE meetings SET capture_mode = ?2 WHERE id = ?1", params![id, mode.as_db()])?;
        Ok(())
    }

    /// User edits (title, notes) and watcher facts (participants, own Teams name). A title set here counts
    /// as edited by the user. New notes mark every enhanced note of the meeting stale.
    pub fn meeting_update(
        &self,
        id: &str,
        title: Option<&str>,
        my_notes: Option<&str>,
        participants: Option<&[String]>,
        teams_self_name: Option<&str>,
    ) -> StoreResult<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let exists: Option<i64> =
            tx.query_row("SELECT 1 FROM meetings WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        if exists.is_none() {
            return Err(StoreError::NotFound);
        }
        if let Some(t) = title {
            let t = t.trim();
            if t.is_empty() {
                return Err(StoreError::Invalid("A meeting needs a title.".into()));
            }
            tx.execute("UPDATE meetings SET title = ?2, title_edited = 1 WHERE id = ?1", params![id, t])?;
        }
        if let Some(n) = my_notes {
            let old: String = tx.query_row("SELECT my_notes FROM meetings WHERE id = ?1", [id], |r| r.get(0))?;
            if old != n {
                tx.execute("UPDATE meetings SET my_notes = ?2 WHERE id = ?1", params![id, n])?;
                tx.execute("UPDATE meeting_notes SET stale = 1 WHERE meeting_id = ?1", [id])?;
            }
        }
        if let Some(p) = participants {
            let json = serde_json::to_string(p).unwrap_or_else(|_| "[]".into());
            tx.execute("UPDATE meetings SET participants_json = ?2 WHERE id = ?1", params![id, json])?;
        }
        if let Some(s) = teams_self_name {
            tx.execute("UPDATE meetings SET teams_self_name = ?2 WHERE id = ?1", params![id, s])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Title read from the Teams window: applied only while the user hasn't edited the title.
    pub fn meeting_set_auto_title(&self, id: &str, title: &str) -> StoreResult<bool> {
        let conn = self.conn()?;
        let n = conn.execute(
            "UPDATE meetings SET title = ?2 WHERE id = ?1 AND title_edited = 0 AND title <> ?2",
            params![id, title],
        )?;
        Ok(n > 0)
    }

    /// Every meeting, newest first, with its preview line.
    pub fn meeting_list(&self) -> StoreResult<Vec<MeetingRow>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(&format!(
            "SELECT {MEETING_COLS},
                (SELECT markdown FROM meeting_notes n WHERE n.meeting_id = m.id AND n.length = 'medium'),
                (SELECT text FROM meeting_segments s WHERE s.meeting_id = m.id AND s.muted = 0 ORDER BY start_ms, id LIMIT 1)
             FROM meetings m ORDER BY started_at_utc DESC, rowid DESC"
        ))?;
        let rows = st.query_map([], |r| {
            let mut m = meeting_from_row(r)?;
            let medium: Option<String> = r.get(12)?;
            let first: Option<String> = r.get(13)?;
            m.preview = medium.as_deref().and_then(first_summary_line).or(first).map(|s| clip(&s));
            Ok(m)
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn meeting_get(&self, id: &str) -> StoreResult<Option<MeetingRow>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(&format!("SELECT {MEETING_COLS} FROM meetings WHERE id = ?1"), [id], meeting_from_row)
            .optional()?)
    }

    /// Delete a meeting: its rows and its folder (audio).
    pub fn meeting_delete(&self, id: &str, meetings_dir: &Path) -> StoreResult<()> {
        {
            let mut conn = self.conn()?;
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM meeting_segments WHERE meeting_id = ?1", [id])?;
            tx.execute("DELETE FROM meeting_notes WHERE meeting_id = ?1", [id])?;
            let n = tx.execute("DELETE FROM meetings WHERE id = ?1", [id])?;
            tx.commit()?;
            if n == 0 {
                return Err(StoreError::NotFound);
            }
            super::checkpoint_truncate(&conn);
        }
        remove_meeting_dir(&meetings_dir.join(id));
        Ok(())
    }

    pub fn segment_insert(&self, s: &NewSegment) -> StoreResult<SegmentRow> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO meeting_segments (meeting_id, source, speaker, confidence, muted, start_ms, end_ms, text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                s.meeting_id,
                s.source.as_db(),
                s.speaker,
                s.confidence as f64,
                s.muted as i64,
                s.start_ms as i64,
                s.end_ms as i64,
                s.text
            ],
        )?;
        let id = conn.last_insert_rowid();
        Ok(conn.query_row(
            "SELECT id, meeting_id, source, speaker, confidence, renamed, muted, start_ms, end_ms, text
             FROM meeting_segments WHERE id = ?1",
            [id],
            segment_from_row,
        )?)
    }

    pub fn segments(&self, meeting_id: &str) -> StoreResult<Vec<SegmentRow>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(
            "SELECT id, meeting_id, source, speaker, confidence, renamed, muted, start_ms, end_ms, text
             FROM meeting_segments WHERE meeting_id = ?1 ORDER BY start_ms, id",
        )?;
        let rows = st.query_map([meeting_id], segment_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Rename one line (`segment_id`) or every line whose speaker is `from` (`None` = unnamed lines).
    /// Returns how many lines changed; any change marks the meeting's notes stale.
    pub fn segment_rename(
        &self,
        meeting_id: &str,
        segment_id: Option<i64>,
        from: Option<&str>,
        to: &str,
    ) -> StoreResult<usize> {
        let to = to.trim();
        if to.is_empty() {
            return Err(StoreError::Invalid("A name can't be empty.".into()));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        let n = match (segment_id, from) {
            (Some(sid), _) => tx.execute(
                "UPDATE meeting_segments SET speaker = ?3, renamed = 1, confidence = 1 WHERE meeting_id = ?1 AND id = ?2",
                params![meeting_id, sid, to],
            )?,
            (None, Some(f)) => tx.execute(
                "UPDATE meeting_segments SET speaker = ?3, renamed = 1, confidence = 1 WHERE meeting_id = ?1 AND speaker = ?2",
                params![meeting_id, f, to],
            )?,
            (None, None) => tx.execute(
                "UPDATE meeting_segments SET speaker = ?2, renamed = 1, confidence = 1
                 WHERE meeting_id = ?1 AND speaker IS NULL AND source = 'them'",
                params![meeting_id, to],
            )?,
        };
        if n > 0 {
            tx.execute("UPDATE meeting_notes SET stale = 1 WHERE meeting_id = ?1", [meeting_id])?;
        }
        tx.commit()?;
        Ok(n)
    }

    /// Insert or replace the note of one length; a saved note is never stale.
    pub fn note_upsert(&self, n: &NoteRow) -> StoreResult<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO meeting_notes (meeting_id, length, markdown, model, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens, cost_usd, created_at_utc, stale)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)",
            params![
                n.meeting_id,
                n.length.as_db(),
                n.markdown,
                n.model,
                n.input_tokens as i64,
                n.output_tokens as i64,
                n.cache_read_tokens as i64,
                n.cache_write_tokens as i64,
                n.cost_usd,
                n.created_at_utc
            ],
        )?;
        Ok(())
    }

    pub fn notes(&self, meeting_id: &str) -> StoreResult<Vec<NoteRow>> {
        let conn = self.conn()?;
        let mut st = conn.prepare(
            "SELECT meeting_id, length, markdown, model, input_tokens, output_tokens, cache_read_tokens,
                    cache_write_tokens, cost_usd, created_at_utc, stale
             FROM meeting_notes WHERE meeting_id = ?1
             ORDER BY CASE length WHEN 'short' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END",
        )?;
        let rows = st.query_map([meeting_id], note_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Estimated spend and number of notes created at or after `since`.
    pub fn notes_cost_since(&self, since: DateTime<Utc>) -> StoreResult<(f64, u32)> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COALESCE(SUM(cost_usd), 0), COUNT(*) FROM meeting_notes WHERE created_at_utc >= ?1",
            [ts(since)],
            |r| Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)? as u32)),
        )?)
    }

    /// At startup: meetings left `recording`/`paused` by a crash or forced quit become `interrupted`.
    /// Their end time is the end of their last segment (or the start time when there is none).
    pub fn mark_interrupted(&self) -> StoreResult<usize> {
        let conn = self.conn()?;
        let ids: Vec<(String, String)> = {
            let mut st = conn.prepare(
                "SELECT id, started_at_utc FROM meetings WHERE status IN ('recording', 'paused')",
            )?;
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (id, started) in &ids {
            let last_end: i64 = conn.query_row(
                "SELECT COALESCE(MAX(end_ms), 0) FROM meeting_segments WHERE meeting_id = ?1",
                [id],
                |r| r.get(0),
            )?;
            let start = DateTime::parse_from_rfc3339(started).map(|t| t.with_timezone(&Utc)).unwrap_or_else(|_| Utc::now());
            let ended = start + Duration::milliseconds(last_end);
            conn.execute(
                "UPDATE meetings SET status = 'interrupted', ended_at_utc = COALESCE(ended_at_utc, ?2),
                    duration_ms = MAX(duration_ms, ?3) WHERE id = ?1",
                params![id, ts(ended), last_end],
            )?;
        }
        Ok(ids.len())
    }

    /// Retention (spec §5.5): delete the audio of meetings that ended more than
    /// `MEETING_AUDIO_RETENTION_HOURS` ago, and folders in `meetings_dir` that no meeting owns (once
    /// `ORPHAN_AUDIO_MIN_AGE_MS` old). Returns how many meetings lost their audio.
    pub fn meeting_sweep(&self, now: DateTime<Utc>, meetings_dir: &Path) -> StoreResult<usize> {
        let cutoff = ts(now - Duration::hours(MEETING_AUDIO_RETENTION_HOURS));
        let (expired, known): (Vec<String>, std::collections::HashSet<String>) = {
            let conn = self.conn()?;
            let expired = {
                let mut st = conn.prepare(
                    "SELECT id FROM meetings WHERE audio_deleted_at IS NULL AND ended_at_utc IS NOT NULL
                       AND status IN ('ended', 'interrupted') AND ended_at_utc < ?1",
                )?;
                st.query_map([&cutoff], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?
            };
            let known = {
                let mut st = conn.prepare("SELECT id FROM meetings")?;
                st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?
            };
            (expired, known)
        };
        for id in &expired {
            delete_wavs(&meetings_dir.join(id));
            let conn = self.conn()?;
            conn.execute("UPDATE meetings SET audio_deleted_at = ?2 WHERE id = ?1", params![id, ts(now)])?;
        }
        let min_age = Duration::milliseconds(ORPHAN_AUDIO_MIN_AGE_MS as i64);
        if let Ok(entries) = std::fs::read_dir(meetings_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                if !path.is_dir() || known.contains(&name) {
                    continue;
                }
                let old_enough = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .map(|m| now - DateTime::<Utc>::from(m) >= min_age)
                    .unwrap_or(false);
                if old_enough {
                    remove_meeting_dir(&path);
                }
            }
        }
        Ok(expired.len())
    }
}

fn delete_wavs(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            delete_wavs(&p);
            let _ = std::fs::remove_dir(&p);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) {
            remove_audio(&p);
        }
    }
}

fn remove_meeting_dir(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error_kind = ?e.kind(), "could not delete a meeting folder"),
    }
}
