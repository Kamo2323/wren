// Typed wrappers for the meeting commands and events (src-tauri/src/meetings/commands.rs). Outside Tauri
// they run on an in-memory mock so the pages can be built and checked in a browser. Mock switches:
// `?meeting=recording` (a live meeting), `?big=1` (2,500-line transcript), `?claude=nokey|error`,
// `?mock=empty` (no meetings).
import { inTauri } from "./api";
import type { ClaudeSpend } from "./meetingText";
import type {
  ActiveMeeting,
  Length,
  MeetingDetail,
  MeetingEvent,
  MeetingRow,
  NoteEvent,
  NoteRow,
  SegmentRow,
  Source,
} from "./meetingTypes";

export * from "./meetingTypes";

type Unlisten = () => void;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

async function on<T>(event: string, handler: (payload: T) => void): Promise<Unlisten> {
  if (!inTauri) {
    const set = mockListeners.get(event) ?? new Set();
    mockListeners.set(event, set);
    const h = handler as (p: unknown) => void;
    set.add(h);
    return () => set.delete(h);
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen<T>(event, (e) => handler(e.payload));
}

export const MEETING_EVENT = "meeting://event";
export const NOTE_EVENT = "meeting://note";
export const CONFIRM_QUIT_EVENT = "meeting://confirm-quit";

export const onMeetingEvent = (h: (e: MeetingEvent) => void) => on<MeetingEvent>(MEETING_EVENT, h);
export const onNoteEvent = (h: (e: NoteEvent) => void) => on<NoteEvent>(NOTE_EVENT, h);
export const onConfirmQuit = (h: () => void) => on<unknown>(CONFIRM_QUIT_EVENT, () => h());

// ---------------------------------------------------------------- mock

const params = typeof location !== "undefined" ? new URLSearchParams(location.search) : new URLSearchParams();
const mockListeners = new Map<string, Set<(p: unknown) => void>>();
const emit = (event: string, payload: unknown) => mockListeners.get(event)?.forEach((h) => h(payload));
const later = <T,>(value: T, ms = 40) => new Promise<T>((r) => setTimeout(() => r(value), ms));
const clone = <T,>(v: T): T => structuredClone(v);

const LINES: Array<[Source, string | null, string]> = [
  ["them", "Sarah Jones", "Morning everyone, thanks for joining. Quick one today: the rollout date and QA."],
  ["me", null, "Morning. I've got the notes from last week open."],
  ["them", "Sarah Jones", "So the rollout moves to Thursday, assuming QA signs off by Wednesday evening."],
  ["me", null, "Fine by me, as long as QA signs off."],
  ["them", "Tom Reid", "I'll chase QA today and send round the test report."],
  ["them", null, "Can we also check the supplier pricing before Thursday?"],
  ["them", "Sarah Jones", "Good point. Maximilian, can you pull the latest pricing sheet?"],
  ["me", null, "Yes, I'll have it by tomorrow lunchtime."],
];

function lines(meetingId: string, count: number, startId: number): SegmentRow[] {
  const out: SegmentRow[] = [];
  for (let i = 0; i < count; i++) {
    const [source, speaker, text] = LINES[i % LINES.length];
    const startMs = 4_000 + i * 6_500;
    out.push({
      id: startId + i,
      meetingId,
      source,
      speaker: source === "me" ? "Maximilian" : speaker,
      confidence: speaker ? 0.92 : 0,
      renamed: false,
      muted: source === "me" && i === 1 && count < 50,
      startMs,
      endMs: startMs + 5_000,
      text: count > LINES.length ? `${text} (${i + 1})` : text,
    });
  }
  return out;
}

const MEDIUM = `> **In short:** The rollout moves to **Thursday**, if QA signs off by **Wednesday evening**.

## Summary
The team agreed to move the rollout to **Thursday**, provided QA signs off by **Wednesday evening**. Tom will chase QA today, and Maximilian will pull the latest supplier pricing ([pricing sheet](https://example.com/pricing)).

## Topics discussed
### Rollout date
- Rollout moves to Thursday.
- Depends on QA sign-off by Wednesday evening.

### Supplier pricing
- Someone asked to check supplier pricing before Thursday.

## Decisions
- Rollout on Thursday, if QA signs off.

## Action items
| Owner | Task | Due |
| --- | --- | --- |
| **Tom Reid** | Chase QA and send round the test report | Today |
| **Maximilian** | Pull the latest supplier pricing sheet | Tomorrow lunchtime |

> "As long as QA signs off, Thursday works." — Maximilian

<img src="https://example.com/tracker.png" alt=""><video src="https://example.com/v.mp4"></video>

## Open questions
None.`;

function ago(ms: number) {
  return new Date(Date.now() - ms).toISOString();
}

interface MockMeeting {
  row: MeetingRow;
  segments: SegmentRow[];
  notes: NoteRow[];
}

const big = params.get("big") === "1";
const recording = params.get("meeting") === "recording";

function mockMeeting(id: string, title: string, startedAgoMs: number, durationMs: number, count: number, withNote: boolean): MockMeeting {
  const row: MeetingRow = {
    id,
    title,
    titleEdited: false,
    startedAtUtc: ago(startedAgoMs),
    endedAtUtc: ago(startedAgoMs - durationMs),
    durationMs,
    status: "ended",
    captureMode: "teams_only",
    myNotes: "",
    participants: ["Sarah Jones", "Tom Reid"],
    audioAvailable: startedAgoMs < 86_400_000,
    preview: withNote ? "The team agreed to move the rollout to Thursday, provided QA signs off by Wednesday evening." : null,
  };
  const segments = lines(id, count, Number(id.replace(/\D/g, "") || 1) * 10_000);
  const notes: NoteRow[] = withNote
    ? [
        {
          meetingId: id,
          length: "medium",
          markdown: MEDIUM,
          model: "claude-opus-5-5",
          inputTokens: 1200,
          outputTokens: 800,
          cacheReadTokens: 0,
          cacheWriteTokens: 16_200,
          costUsd: 0.1134,
          createdAtUtc: row.endedAtUtc ?? row.startedAtUtc,
          stale: false,
        },
      ]
    : [];
  if (!withNote) row.preview = segments[0]?.text ?? null;
  return { row, segments, notes };
}

const mockStore: { meetings: MockMeeting[]; active: ActiveMeeting | null; key: boolean; nextSeg: number } = {
  meetings:
    params.get("mock") === "empty"
      ? []
      : [
          mockMeeting("m1", "Weekly sync with Sarah", 2 * 3_600_000, 32 * 60_000, big ? 2500 : 8, true),
          mockMeeting("m2", "Supplier call", 26 * 3_600_000, 48 * 60_000, 8, false),
          mockMeeting("m3", "Quarterly planning", 50 * 3_600_000, 71 * 60_000, 8, true),
        ],
  active: null,
  key: params.get("claude") !== "nokey",
  nextSeg: 900_000,
};

if (recording) {
  const live = mockMeeting("live", "Design review", 12 * 60_000, 0, big ? 2500 : 6, false);
  live.row.status = "recording";
  live.row.endedAtUtc = null;
  mockStore.meetings.unshift(live);
  mockStore.active = {
    id: "live",
    status: "recording",
    startedAtUtc: live.row.startedAtUtc,
    elapsedMs: 12 * 60_000,
    captureMode: "teams_only",
    catchingUp: false,
    speakingMe: false,
    speakingThem: true,
  };
}

let ticker: ReturnType<typeof setInterval> | null = null;
function startTicker() {
  if (ticker || inTauri) return;
  ticker = setInterval(() => {
    const a = mockStore.active;
    if (!a) return;
    if (a.status === "recording") a.elapsedMs += 1000;
    emit(MEETING_EVENT, { kind: "status", active: clone(a) } satisfies MeetingEvent);
  }, 1000);
}
if (recording) startTicker();

function find(id: string) {
  return mockStore.meetings.find((m) => m.row.id === id);
}

function preview(m: MockMeeting) {
  const medium = m.notes.find((n) => n.length === "medium");
  const line = medium?.markdown.split("\n").find((l) => l.trim() && !l.startsWith("#"));
  const clean = line?.replace(/^[>\-*]\s*/, "").replace(/\*\*/g, "").replace(/^In short:\s*/, "");
  return clean ?? m.segments.find((s) => !s.muted)?.text ?? null;
}

function mockEnhance(id: string, length: Length) {
  const m = find(id);
  if (!m) return;
  const mode = params.get("claude");
  if (!mockStore.key) {
    setTimeout(() => emit(NOTE_EVENT, { event: "error", meetingId: id, length, kind: "no_key", message: "Add your Claude API key in Settings." }), 50);
    return;
  }
  emit(NOTE_EVENT, { event: "started", meetingId: id, length } satisfies NoteEvent);
  const text =
    length === "short"
      ? MEDIUM.split("## Topics")[0].trim() + "\n\n## Action items\n| Owner | Task | Due |\n| --- | --- | --- |\n| **Tom Reid** | Chase QA | Today |"
      : MEDIUM;
  const chunks = text.match(/[\s\S]{1,40}/g) ?? [];
  chunks.forEach((c, i) =>
    setTimeout(() => {
      if (mode === "error" && i === 3) {
        emit(NOTE_EVENT, { event: "error", meetingId: id, length, kind: "network", message: "Couldn't reach Claude (the connection dropped)." });
        return;
      }
      if (mode === "error" && i > 3) return;
      emit(NOTE_EVENT, { event: "delta", meetingId: id, length, text: c });
      if (i === chunks.length - 1) {
        const note: NoteRow = {
          meetingId: id,
          length,
          markdown: text,
          model: "claude-opus-5-5",
          inputTokens: 1100,
          outputTokens: 600,
          cacheReadTokens: 16_000,
          cacheWriteTokens: 0,
          costUsd: 0.0198,
          createdAtUtc: new Date().toISOString(),
          stale: false,
        };
        m.notes = m.notes.filter((n) => n.length !== length).concat(note);
        m.row.preview = preview(m);
        emit(NOTE_EVENT, { event: "done", note: clone(note) });
      }
    }, 30 + i * 25),
  );
}

// ---------------------------------------------------------------- commands

export const meetingList = (): Promise<MeetingRow[]> =>
  inTauri ? call("meeting_list") : later(mockStore.meetings.map((m) => clone(m.row)).sort((a, b) => b.startedAtUtc.localeCompare(a.startedAtUtc)));

export const meetingGet = (id: string): Promise<MeetingDetail | null> => {
  if (inTauri) return call("meeting_get", { id });
  const m = find(id);
  return later(m ? clone({ meeting: m.row, segments: m.segments, notes: m.notes }) : null);
};

export const meetingActive = (): Promise<ActiveMeeting | null> =>
  inTauri ? call("meeting_active") : later(mockStore.active ? clone(mockStore.active) : null);

export const meetingStart = (title?: string): Promise<MeetingRow> => {
  if (inTauri) return call("meeting_start", { title: title ?? null });
  if (mockStore.active) return Promise.reject(new Error("A meeting is already being recorded."));
  const m = mockMeeting(`n${Date.now()}`, title || `Meeting ${new Date().toLocaleDateString("en-GB", { day: "numeric", month: "short" })}`, 0, 0, 0, false);
  m.row.status = "recording";
  m.row.endedAtUtc = null;
  m.row.preview = null;
  mockStore.meetings.unshift(m);
  mockStore.active = {
    id: m.row.id,
    status: "recording",
    startedAtUtc: m.row.startedAtUtc,
    elapsedMs: 0,
    captureMode: "teams_only",
    catchingUp: false,
    speakingMe: false,
    speakingThem: false,
  };
  startTicker();
  emit(MEETING_EVENT, { kind: "status", active: clone(mockStore.active) });
  return later(clone(m.row));
};

function setMockStatus(status: "recording" | "paused") {
  if (!mockStore.active) return Promise.reject(new Error("No meeting is being recorded."));
  mockStore.active.status = status;
  mockStore.active.speakingThem = false;
  const m = find(mockStore.active.id);
  if (m) m.row.status = status;
  emit(MEETING_EVENT, { kind: "status", active: clone(mockStore.active) });
  return later(undefined);
}

export const meetingPause = (): Promise<void> => (inTauri ? call("meeting_pause") : setMockStatus("paused"));
export const meetingResume = (): Promise<void> => (inTauri ? call("meeting_resume") : setMockStatus("recording"));

export const meetingStop = (): Promise<string | null> => {
  if (inTauri) return call("meeting_stop");
  const a = mockStore.active;
  if (!a) return later(null);
  const m = find(a.id);
  if (m) {
    m.row.status = "ended";
    m.row.endedAtUtc = new Date().toISOString();
    m.row.durationMs = a.elapsedMs;
  }
  mockStore.active = null;
  emit(MEETING_EVENT, { kind: "status", active: null });
  if (mockStore.key) mockEnhance(a.id, "medium");
  return later(a.id, 120);
};

export const meetingStopAndQuit = (): Promise<void> => (inTauri ? call("meeting_stop_and_quit") : meetingStop().then(() => undefined));

if (!inTauri) window.addEventListener("wren-mock-stop-meeting", () => void meetingStop());

let notesSaveFailed = false;
export const meetingUpdate = (id: string, patch: { title?: string; myNotes?: string }): Promise<void> => {
  if (inTauri) return call("meeting_update", { id, title: patch.title ?? null, myNotes: patch.myNotes ?? null });
  const notesSave = params.get("notesSave");
  if (patch.myNotes !== undefined) window.dispatchEvent(new Event("wren-mock-notes-saving"));
  if (patch.myNotes !== undefined && (notesSave === "fail" || (notesSave === "fail-once" && !notesSaveFailed))) {
    notesSaveFailed = true;
    return Promise.reject(new Error("Couldn't save My notes."));
  }
  const m = find(id);
  if (m) {
    if (patch.title !== undefined) {
      m.row.title = patch.title;
      m.row.titleEdited = true;
    }
    if (patch.myNotes !== undefined && patch.myNotes !== m.row.myNotes) {
      m.row.myNotes = patch.myNotes;
      m.notes.forEach((n) => (n.stale = true));
    }
  }
  return later(undefined);
};

export const meetingRenameSpeaker = (id: string, to: string, segmentId: number | null, from: string | null): Promise<number> => {
  if (inTauri) return call("meeting_rename_speaker", { id, segmentId, from, to });
  const m = find(id);
  let n = 0;
  m?.segments.forEach((s) => {
    const hit = segmentId !== null ? s.id === segmentId : from === null ? s.speaker === null && s.source === "them" : s.speaker === from;
    if (hit) {
      s.speaker = to;
      s.renamed = true;
      s.confidence = 1;
      n++;
    }
  });
  if (n && m) m.notes.forEach((x) => (x.stale = true));
  return later(n);
};

export const meetingDelete = async (id: string): Promise<void> => {
  if (inTauri) await call("meeting_delete", { id });
  else {
    mockStore.meetings = mockStore.meetings.filter((m) => m.row.id !== id);
    await later(undefined);
  }
  notesDrafts.delete(id);
};

export const meetingAudioClip = (id: string, source: Source, startMs: number, endMs: number): Promise<string | null> =>
  inTauri ? call("meeting_audio_clip", { id, source, startMs, endMs }) : later(null);

/** "started", "running" (already being written) or "exists" (kept; nothing sent). */
export type EnhanceStart = "started" | "running" | "exists";

export const meetingEnhance = (id: string, length: Length, regenerate: boolean): Promise<EnhanceStart> => {
  if (inTauri) return call("meeting_enhance", { id, length, regenerate });
  const m = find(id);
  if (!regenerate && m?.notes.some((n) => n.length === length)) return later("exists" as const);
  mockEnhance(id, length);
  return later("started" as const, 10);
};

export const meetingGenerating = (id: string): Promise<Length[]> => (inTauri ? call("meeting_generating", { id }) : later([]));

export const claudeKeyStatus = (): Promise<{ saved: boolean }> => (inTauri ? call("claude_key_status") : later({ saved: mockStore.key }));

export const claudeKeySave = (key: string): Promise<void> => {
  if (inTauri) return call("claude_key_save", { key });
  if (!key.trim().startsWith("sk-ant-") || key.trim().length < 40) {
    return Promise.reject(new Error("That doesn't look like a Claude API key (they start with sk-ant-)."));
  }
  mockStore.key = true;
  return later(undefined, 300);
};

export const claudeKeyRemove = (): Promise<void> => {
  if (inTauri) return call("claude_key_remove");
  mockStore.key = false;
  return later(undefined);
};

export const claudeSpendMonth = (): Promise<ClaudeSpend> =>
  inTauri ? call("claude_spend_month") : later({ usd: 2.4, notes: 14, cleanupUsd: 0.03, cleanups: 140 });

/** Fired on `window` when the Claude key is saved or forgotten, so every card that depends on it updates. */
export const CLAUDE_KEY_EVENT = "wren:claude-key-changed";

export const systemAudioPermission = (): Promise<"granted" | "denied" | "unknown"> =>
  inTauri ? call("system_audio_permission") : later(params.get("sysaudio") === "denied" ? "denied" : "granted");

export const openSystemAudioSettings = (): Promise<void> => (inTauri ? call("open_system_audio_settings") : later(undefined));

/** Errors from invoke arrive as strings; mocks reject with Error. */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return "Something went wrong.";
}

type NotesDraft = { json: string; saving?: { json: string; result: Promise<string> } };
// Keep drafts and in-flight writes across editor and page unmounts so quit can retry a failed save.
const notesDrafts = new Map<string, NotesDraft>();

export function getNotesDraft(id: string) {
  return notesDrafts.get(id)?.json;
}

export function setNotesDraft(id: string, json: string) {
  const draft = notesDrafts.get(id);
  if (draft) draft.json = json;
  else notesDrafts.set(id, { json });
}

export async function saveNotesDraft(id: string): Promise<string | undefined> {
  const draft = notesDrafts.get(id);
  if (!draft) return;
  if (draft.saving) {
    if (draft.saving.json === draft.json) return draft.saving.result;
    // Finish the previous write before sending the newer edit, even if that write failed.
    await draft.saving.result.catch(() => {});
    return saveNotesDraft(id);
  }
  const json = draft.json;
  const result = meetingUpdate(id, { myNotes: json }).then(() => {
    if (notesDrafts.get(id) === draft && draft.json === json) notesDrafts.delete(id);
    return json;
  }).finally(() => {
    draft.saving = undefined;
  });
  draft.saving = { json, result };
  return result;
}

let notesFlush: { id: string; flush: () => Promise<void> } | null = null;

/** The open meeting can also cancel the editor's debounce and update its save indicator. */
export function setNotesFlush(current: typeof notesFlush) {
  notesFlush = current;
}

/** Save pending drafts and report failure or timeout so quit can ask before proceeding. */
export async function flushPendingNotes(maxMs = 2_000): Promise<boolean> {
  if (!notesDrafts.size) return true;
  const current = notesFlush;
  const pending = [...notesDrafts.keys()].map((id) => current?.id === id ? current.flush() : saveNotesDraft(id));
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      Promise.all(pending).then(() => true, () => false),
      new Promise<boolean>((resolve) => { timer = setTimeout(() => resolve(false), maxMs); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}
