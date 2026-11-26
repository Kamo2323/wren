// Pure helpers for the Meetings UI (unit-tested in scripts/unit-tests.mjs). No Tauri imports here.
import type { CaptureMode, NoteRow, SegmentRow } from "./meetingTypes";

/** Estimated spend this month: enhanced notes and AI clean-up, counted apart (`claude_spend_month`). */
export interface ClaudeSpend {
  usd: number;
  notes: number;
  cleanupUsd: number;
  cleanups: number;
}

/** "~$2.40 across 14 notes · ~$0.03 across 140 clean-ups (estimated from list prices)." */
export function spendLine(spend: ClaudeSpend): string {
  const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;
  const parts = [`~$${spend.usd.toFixed(2)} across ${plural(spend.notes, "note", "notes")}`];
  if (spend.cleanups > 0) parts.push(`~$${spend.cleanupUsd.toFixed(2)} across ${plural(spend.cleanups, "clean-up", "clean-ups")}`);
  return `${parts.join(" · ")} (estimated from list prices).`;
}


/** Index 0..5 into the speaker accent palette, stable per name. */
export function speakerColour(name: string): number {
  let h = 2166136261;
  for (let i = 0; i < name.length; i++) {
    h ^= name.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return Math.abs(h) % 6;
}

/** Accent colour per speaker for one meeting: the user always 0, everyone else in the order they first
 *  speak (1..5, then wrapping), so up to six people never share a colour. */
export function assignColours(namesInOrder: string[], meName: string): Map<string, number> {
  const m = new Map<string, number>([[meName, 0]]);
  let next = 1;
  for (const n of namesInOrder) {
    if (!m.has(n)) {
      m.set(n, next);
      next = next >= 5 ? 1 : next + 1;
    }
  }
  return m;
}

/** The name shown on a line: the stored name, else the user's meeting name (Me) or "Speaker". */
export function displayName(s: Pick<SegmentRow, "speaker" | "source">, meName: string): string {
  if (s.speaker && s.speaker.trim()) return s.speaker;
  return s.source === "me" ? meName : "Speaker";
}

export interface Run {
  key: string;
  source: SegmentRow["source"];
  speaker: string | null;
  startMs: number;
  lines: SegmentRow[];
}

/** A run of one speaker's consecutive lines breaks after this much silence… */
const RUN_GAP_MS = 60_000;
/** …or at this many lines, so a long monologue still renders in windowed pieces. */
const MAX_RUN_LINES = 40;

/** Consecutive lines from one speaker (same source and same stored name) become one run. */
export function groupRuns(segments: SegmentRow[]): Run[] {
  const runs: Run[] = [];
  for (const s of segments) {
    const last = runs[runs.length - 1];
    const prev = last?.lines[last.lines.length - 1];
    if (
      last &&
      prev &&
      last.source === s.source &&
      last.speaker === s.speaker &&
      s.startMs - prev.endMs < RUN_GAP_MS &&
      last.lines.length < MAX_RUN_LINES
    ) {
      last.lines.push(s);
    } else {
      runs.push({ key: `r${s.id}`, source: s.source, speaker: s.speaker, startMs: s.startMs, lines: [s] });
    }
  }
  return runs;
}

/** "01:05", or "1:02:05" past an hour. */
export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const two = (n: number) => String(n).padStart(2, "0");
  return s >= 3600 ? `${Math.floor(s / 3600)}:${two(Math.floor(s / 60) % 60)}:${two(s % 60)}` : `${two(Math.floor(s / 60))}:${two(s % 60)}`;
}

/** Wall-clock time of a line: the meeting start plus its offset, as "10:02". */
export function clockAt(startedAtUtc: string, offsetMs: number): string {
  const t = new Date(new Date(startedAtUtc).getTime() + offsetMs);
  return t.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
}

export function captureNote(mode: CaptureMode): string {
  switch (mode) {
    case "teams_only":
      return "Teams only";
    case "all_sound":
      return "Capturing all computer sound";
    case "mic_only":
      return "Teams isn't open — only your mic is being recorded";
  }
}

export function modelLabel(model: string): string {
  return model === "claude-sonnet-5-5" ? "Claude Sonnet 5.5" : "Claude Opus 5.5";
}

/** "Claude Opus 5.5 · 18.2k tokens · ~$0.11 · 14:32" */
export function footerLabel(
  n: Pick<NoteRow, "model" | "inputTokens" | "outputTokens" | "cacheReadTokens" | "cacheWriteTokens" | "costUsd" | "createdAtUtc">,
  timeOf: (iso: string) => string = (iso) => new Date(iso).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" }),
): string {
  const tokens = n.inputTokens + n.outputTokens + n.cacheReadTokens + n.cacheWriteTokens;
  const k = tokens >= 1000 ? `${(tokens / 1000).toFixed(1)}k` : String(tokens);
  return `${modelLabel(n.model)} · ${k} tokens · ~$${n.costUsd.toFixed(2)} · ${timeOf(n.createdAtUtc)}`;
}

/** "32 min", "1 h 05 min". */
export function durationLabel(ms: number): string {
  const m = Math.max(1, Math.round(ms / 60_000));
  return m >= 60 ? `${Math.floor(m / 60)} h ${String(m % 60).padStart(2, "0")} min` : `${m} min`;
}

export function participantsLabel(names: string[]): string {
  if (names.length === 0) return "";
  if (names.length <= 2) return names.join(", ");
  return `${names.length} people`;
}
