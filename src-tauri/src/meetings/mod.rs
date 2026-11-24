//! Meetings (spec `docs/superpowers/specs/2026-11-24-meetings-design.md`): record a Teams meeting as two
//! streams (Me = microphone, Them = Teams' audio), transcribe it live with the dictation engine, name the
//! speakers from Teams, and write enhanced notes with Claude.
//!
//! Text from meetings is never logged; logs carry counts and ids only.

use serde::{Deserialize, Serialize};

/// Which stream a segment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Me,
    Them,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Paused,
    Ended,
    Interrupted,
}

/// Enhanced-notes length (spec §5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Length {
    Short,
    Medium,
    Long,
}

/// How "Them" audio is being captured (spec §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    /// Teams' own audio only (process loopback / process tap).
    TeamsOnly,
    /// Fallback: everything the computer plays.
    AllSound,
    /// Teams isn't running: microphone only.
    MicOnly,
}

macro_rules! db_enum {
    ($t:ty { $($v:ident => $s:literal),+ $(,)? }) => {
        impl $t {
            pub fn as_db(self) -> &'static str {
                match self { $(Self::$v => $s),+ }
            }
            pub fn from_db(s: &str) -> Option<Self> {
                match s { $($s => Some(Self::$v),)+ _ => None }
            }
        }
    };
}
db_enum!(Source { Me => "me", Them => "them" });
db_enum!(MeetingStatus { Recording => "recording", Paused => "paused", Ended => "ended", Interrupted => "interrupted" });
db_enum!(Length { Short => "short", Medium => "medium", Long => "long" });
db_enum!(CaptureMode { TeamsOnly => "teams_only", AllSound => "all_sound", MicOnly => "mic_only" });

/// One meeting as the UI sees it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRow {
    pub id: String,
    pub title: String,
    /// True once the user edited the title; Teams' window title no longer replaces it.
    pub title_edited: bool,
    pub started_at_utc: String,
    pub ended_at_utc: Option<String>,
    pub duration_ms: u64,
    pub status: MeetingStatus,
    pub capture_mode: CaptureMode,
    /// TipTap document JSON ("" when empty).
    pub my_notes: String,
    pub participants: Vec<String>,
    /// The user's own Teams display name (never shown in the UI; spec §5.3).
    #[serde(skip_serializing)]
    pub teams_self_name: Option<String>,
    pub audio_available: bool,
    pub preview: Option<String>,
}

/// A transcribed segment to save.
#[derive(Debug, Clone, PartialEq)]
pub struct NewSegment {
    pub meeting_id: String,
    pub source: Source,
    pub speaker: Option<String>,
    pub confidence: f32,
    pub muted: bool,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentRow {
    pub id: i64,
    pub meeting_id: String,
    pub source: Source,
    pub speaker: Option<String>,
    pub confidence: f32,
    pub renamed: bool,
    pub muted: bool,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteRow {
    pub meeting_id: String,
    pub length: Length,
    pub markdown: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub created_at_utc: String,
    pub stale: bool,
}

pub mod dedupe;
pub mod segmenter;
pub mod speakers;
pub mod teams;
pub mod capture;
