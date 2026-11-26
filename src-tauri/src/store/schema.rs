//! Database schema and migrations (spec §9). Migrations run in order, each in its own transaction,
//! and `PRAGMA user_version` records how many have run.

use rusqlite::Connection;

/// Each entry migrates from version `i` to `i + 1`. Never edit a released migration; add one.
pub const MIGRATIONS: &[&str] = &[
    // v1 — first schema.
    r#"
    CREATE TABLE transcriptions (
        id               TEXT PRIMARY KEY,
        created_at_utc   TEXT NOT NULL,
        local_date       TEXT NOT NULL,
        local_hour       INTEGER NOT NULL CHECK (local_hour BETWEEN 0 AND 23),
        mode             TEXT NOT NULL CHECK (mode IN ('hold', 'handsfree')),
        audio_path       TEXT,
        duration_ms      INTEGER NOT NULL,
        speech_ms        INTEGER NOT NULL,
        raw_text         TEXT NOT NULL,
        tidied_text      TEXT NOT NULL,
        final_text       TEXT NOT NULL,
        words            INTEGER NOT NULL,
        model            TEXT NOT NULL,
        device           TEXT NOT NULL,
        timings_json     TEXT NOT NULL,
        target_process   TEXT NOT NULL,
        paste_outcome    TEXT NOT NULL
                         CHECK (paste_outcome IN ('pasted', 'not_pasted', 'copied', 'failed')),
        applied_rule_ids TEXT NOT NULL DEFAULT '[]'
    );
    CREATE INDEX idx_transcriptions_created ON transcriptions (created_at_utc);

    CREATE TABLE dictionary_entries (
        id                      INTEGER PRIMARY KEY AUTOINCREMENT,
        kind                    TEXT NOT NULL CHECK (kind IN ('term', 'replacement')),
        from_text               TEXT,
        to_text                 TEXT NOT NULL,
        source                  TEXT NOT NULL
                                CHECK (source IN ('willow_seed', 'manual', 'learned_history', 'learned_paste')),
        status                  TEXT NOT NULL CHECK (status IN ('candidate', 'active', 'disabled')),
        observations            INTEGER NOT NULL DEFAULT 1,
        hits                    INTEGER NOT NULL DEFAULT 0,
        origin_transcription_id TEXT,
        batch_id                TEXT,
        created_at              TEXT NOT NULL,
        updated_at              TEXT NOT NULL,
        CHECK ((kind = 'term' AND from_text IS NULL) OR (kind = 'replacement' AND from_text IS NOT NULL))
    );
    CREATE INDEX idx_dictionary_lookup
        ON dictionary_entries (kind, from_text COLLATE NOCASE, to_text COLLATE NOCASE);

    CREATE TABLE correction_events (
        id               INTEGER PRIMARY KEY AUTOINCREMENT,
        transcription_id TEXT NOT NULL,
        from_text        TEXT NOT NULL,
        to_text          TEXT NOT NULL,
        source           TEXT NOT NULL,
        decision         TEXT NOT NULL,
        created_at       TEXT NOT NULL
    );
    CREATE INDEX idx_correction_events_transcription ON correction_events (transcription_id);

    -- Kept forever: holds counts only, never text.
    CREATE TABLE hourly_totals (
        local_date  TEXT NOT NULL,
        hour        INTEGER NOT NULL CHECK (hour BETWEEN 0 AND 23),
        dictations  INTEGER NOT NULL DEFAULT 0,
        words       INTEGER NOT NULL DEFAULT 0,
        speaking_ms INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (local_date, hour)
    );
    "#,
    // v2 — meetings (docs/superpowers/specs/2026-11-24-meetings-design.md §5.5).
    r#"
    CREATE TABLE meetings (
        id                TEXT PRIMARY KEY,
        title             TEXT NOT NULL,
        title_edited      INTEGER NOT NULL DEFAULT 0,
        started_at_utc    TEXT NOT NULL,
        ended_at_utc      TEXT,
        duration_ms       INTEGER NOT NULL DEFAULT 0,
        status            TEXT NOT NULL CHECK (status IN ('recording', 'paused', 'ended', 'interrupted')),
        capture_mode      TEXT NOT NULL CHECK (capture_mode IN ('teams_only', 'all_sound', 'mic_only')),
        my_notes          TEXT NOT NULL DEFAULT '',
        participants_json TEXT NOT NULL DEFAULT '[]',
        teams_self_name   TEXT,
        audio_deleted_at  TEXT
    );
    CREATE INDEX idx_meetings_started ON meetings (started_at_utc);

    CREATE TABLE meeting_segments (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        meeting_id  TEXT NOT NULL,
        source      TEXT NOT NULL CHECK (source IN ('me', 'them')),
        speaker     TEXT,
        confidence  REAL NOT NULL DEFAULT 0,
        renamed     INTEGER NOT NULL DEFAULT 0,
        muted       INTEGER NOT NULL DEFAULT 0,
        start_ms    INTEGER NOT NULL,
        end_ms      INTEGER NOT NULL,
        text        TEXT NOT NULL
    );
    CREATE INDEX idx_meeting_segments_meeting ON meeting_segments (meeting_id, start_ms);

    CREATE TABLE meeting_notes (
        meeting_id         TEXT NOT NULL,
        length             TEXT NOT NULL CHECK (length IN ('short', 'medium', 'long')),
        markdown           TEXT NOT NULL,
        model              TEXT NOT NULL,
        input_tokens       INTEGER NOT NULL DEFAULT 0,
        output_tokens      INTEGER NOT NULL DEFAULT 0,
        cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
        cache_write_tokens INTEGER NOT NULL DEFAULT 0,
        cost_usd           REAL NOT NULL DEFAULT 0,
        created_at_utc     TEXT NOT NULL,
        stale              INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (meeting_id, length)
    );
    "#,
    // v3 — meetings council fixes: when notes/names last changed (stale detection), and a spend log
    // that keeps the cost of every note written, including ones later regenerated.
    r#"
    ALTER TABLE meetings ADD COLUMN inputs_changed_at TEXT;
    CREATE TABLE meeting_spend (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        meeting_id     TEXT NOT NULL,
        created_at_utc TEXT NOT NULL,
        cost_usd       REAL NOT NULL
    );
    CREATE INDEX idx_meeting_spend_created ON meeting_spend (created_at_utc);
    INSERT INTO meeting_spend (meeting_id, created_at_utc, cost_usd)
        SELECT meeting_id, created_at_utc, cost_usd FROM meeting_notes;
    "#,
    // v4 — AI clean-up through Claude: the estimated cost of every clean-up request (no text).
    r#"
    CREATE TABLE cleanup_spend (
        id             INTEGER PRIMARY KEY AUTOINCREMENT,
        created_at_utc TEXT NOT NULL,
        model          TEXT NOT NULL,
        cost_usd       REAL NOT NULL
    );
    CREATE INDEX idx_cleanup_spend_created ON cleanup_spend (created_at_utc);
    "#,
];

/// Latest schema version this build knows.
pub fn latest_version() -> i64 {
    MIGRATIONS.len() as i64
}

pub fn user_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Outcome of `migrate`.
#[derive(Debug, PartialEq, Eq)]
pub enum Migrated {
    /// Ran migrations `from..to`.
    Ran { from: i64, to: i64 },
    UpToDate,
    /// The file was written by a newer Wren; nothing was touched.
    TooNew { found: i64, known: i64 },
}

pub fn migrate(conn: &mut Connection) -> rusqlite::Result<Migrated> {
    let from = user_version(conn)?;
    let known = latest_version();
    if from > known {
        return Ok(Migrated::TooNew { found: from, known });
    }
    if from == known {
        return Ok(Migrated::UpToDate);
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(from as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
        tx.commit()?;
    }
    Ok(Migrated::Ran { from, to: known })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(conn: &Connection) -> Vec<String> {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
    }

    #[test]
    fn migrates_an_empty_database_to_the_latest_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);
        assert_eq!(migrate(&mut conn).unwrap(), Migrated::Ran { from: 0, to: latest_version() });
        assert_eq!(user_version(&conn).unwrap(), latest_version());
        assert_eq!(
            tables(&conn),
            [
                "cleanup_spend",
                "correction_events",
                "dictionary_entries",
                "hourly_totals",
                "meeting_notes",
                "meeting_segments",
                "meeting_spend",
                "meetings",
                "transcriptions"
            ]
        );
        // Running again is a no-op.
        assert_eq!(migrate(&mut conn).unwrap(), Migrated::UpToDate);
    }

    #[test]
    fn refuses_a_database_from_a_newer_wren() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        assert_eq!(
            migrate(&mut conn).unwrap(),
            Migrated::TooNew { found: 99, known: latest_version() }
        );
        assert!(tables(&conn).is_empty());
    }
}
