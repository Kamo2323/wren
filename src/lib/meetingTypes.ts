// Shapes the Rust side sends (src-tauri/src/meetings): camelCase fields, snake_case enum values.

export type Source = "me" | "them";
export type MeetingStatus = "recording" | "paused" | "ended" | "interrupted";
export type CaptureMode = "teams_only" | "all_sound" | "mic_only";
export type Length = "short" | "medium" | "long";

export interface MeetingRow {
  id: string;
  title: string;
  titleEdited: boolean;
  startedAtUtc: string;
  endedAtUtc: string | null;
  durationMs: number;
  status: MeetingStatus;
  captureMode: CaptureMode;
  /** TipTap JSON ("" when empty). */
  myNotes: string;
  participants: string[];
  audioAvailable: boolean;
  preview: string | null;
}

export interface SegmentRow {
  id: number;
  meetingId: string;
  source: Source;
  speaker: string | null;
  confidence: number;
  renamed: boolean;
  muted: boolean;
  startMs: number;
  endMs: number;
  text: string;
}

export interface NoteRow {
  meetingId: string;
  length: Length;
  markdown: string;
  model: string;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  costUsd: number;
  createdAtUtc: string;
  stale: boolean;
}

export interface MeetingDetail {
  meeting: MeetingRow;
  segments: SegmentRow[];
  notes: NoteRow[];
}

export interface ActiveMeeting {
  id: string;
  status: MeetingStatus;
  startedAtUtc: string;
  elapsedMs: number;
  captureMode: CaptureMode;
  catchingUp: boolean;
  speakingMe: boolean;
  speakingThem: boolean;
}

export type MeetingEvent =
  | { kind: "segment"; segment: SegmentRow }
  | { kind: "speaking"; meetingId: string; source: Source; on: boolean }
  | { kind: "status"; active: ActiveMeeting | null }
  | { kind: "participants"; meetingId: string; names: string[] }
  | { kind: "title"; meetingId: string; title: string };

export type NoteErrorKind = "no_key" | "unauthorized" | "rate_limited" | "overloaded" | "network" | "refusal" | "api";

export type NoteEvent =
  | { event: "started"; meetingId: string; length: Length }
  | { event: "delta"; meetingId: string; length: Length; text: string }
  | { event: "done"; note: NoteRow }
  | { event: "error"; meetingId: string; length: Length; kind: NoteErrorKind; message: string };
