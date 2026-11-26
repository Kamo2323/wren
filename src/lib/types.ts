// Shapes shared with the Rust side. Rust structs use serde camelCase; these mirror them.

/** What the session (Task 8) emits on "popup://state". Rust: enum with serde tag = "kind", camelCase. */
export type PopupState =
  | { kind: "hidden" }
  | { kind: "listening"; cpu: boolean }
  | { kind: "handsfree"; startedAtMs: number; cpu: boolean }
  | { kind: "warming" }
  | { kind: "transcribing"; sinceMs: number }
  | { kind: "pasted" }
  | { kind: "nothing" }
  /** `recording`: shown during a recording (silent mic) — stays until replaced, no countdown. */
  | { kind: "noSound"; recording: boolean }
  | { kind: "notPasted"; text: string }
  | { kind: "learned"; from: string; to: string; entryId: number }
  | { kind: "error"; message: string }
  | { kind: "capWarning"; remainingMs: number; mode: "hold" | "handsFree" };

export type PopupKind = PopupState["kind"];

export interface UiConstants {
  platform: "windows" | "macos";
  retentionHours: number;
  typingWpmDefault: number;
  popupWindowW: number;
  popupWindowH: number;
  recordingWarnMs: number;
  maxRecordingMs: number;
  popupPastedMs: number;
  popupStillWorkingAfterMs: number;
  popupNothingMs: number;
  popupNoSoundMs: number;
  popupNotPastedMs: number;
  popupLearnedMs: number;
  popupErrorMs: number;
}

export interface EngineSettings {
  model: string;
  computeType: string;
  beamSize: number;
  cpuFallbackModel: string;
  language: string;
  vadFilter: boolean;
  conditionOnPreviousText: boolean;
}

export interface AiCleanupSettings {
  enabled: boolean;
  /** "claude-haiku-5-5" (default) | "claude-sonnet-5-5" */
  model: string;
  /** How much Claude may change text going into coding apps. */
  codingStyle: "rewrite" | "tidy";
}

export interface Settings {
  version: number;
  engine: EngineSettings;
  micDevice: string | null;
  alwaysOpenMic: boolean;
  typingWpm: number;
  startWithWindows: boolean;
  sounds: boolean;
  aiCleanup: AiCleanupSettings;
  watcherEnabled: boolean | null;
  willowSeedImported: boolean;
  /** Colour theme preference: `null` / omitted follows Windows, or `"dark"` / `"light"`. */
  theme?: "dark" | "light" | null;
  /** Name on the user's own meeting lines. */
  meetingName: string;
  /** "claude-opus-5-5" | "claude-sonnet-5-5" */
  claudeModel: string;
  /** Save each meeting's Teams readings and lines next to its audio (off by default). */
  saveCallDiagnostics: boolean;
}

/** Task 4 `Mode`. Serialised in camelCase; "handsfree" is accepted too (it is how the DB stores it). */
export type Mode = "hold" | "handsFree" | "handsfree";
export type PasteOutcome = "pasted" | "notPasted" | "copied" | "failed" | "not_pasted";

export interface TranscriptionRow {
  id: string;
  createdAtUtc: string; // RFC 3339
  mode: Mode;
  durationMs: number;
  finalText: string;
  rawText: string;
  words: number;
  targetProcess: string;
  pasteOutcome: PasteOutcome;
}

export interface DayPoint {
  date: string; // YYYY-MM-DD, local
  words: number;
  speakingMs: number; // speaking time that day; 0 = nothing spoken (a gap in the WPM trend)
}

export interface Stats {
  timeSavedMsAll: number;
  timeSavedMsWeek: number;
  wpmToday: number | null;
  wpmAll: number | null;
  streakDays: number;
  busiestHour: number | null;
  wordsToday: number;
  wordsWeek: number;
  wordsAll: number;
  dictationsAll: number;
  daily: DayPoint[]; // 30 entries, oldest first
  hourly: number[]; // 24 entries
}

export type EntryKind = "term" | "replacement";
export type EntrySource = "willowSeed" | "manual" | "learnedHistory" | "learnedPaste";
export type EntryStatus = "candidate" | "active" | "disabled";

export interface DictionaryEntry {
  id: number;
  kind: EntryKind;
  fromText: string | null;
  toText: string;
  source: EntrySource;
  status: EntryStatus;
  observations: number;
  hits: number;
  createdAt: string;
}

/** Result of `history_fix_word`: the replacement it created and, maybe, a new dictionary term. */
export interface FixWordResult {
  replacement: DictionaryEntry;
  term: DictionaryEntry | null;
}

/** Normalised engine status for the UI (see `normaliseEngineStatus` in api.ts). */
export type EngineStatus =
  | { state: "starting" }
  | { state: "warming" }
  | { state: "ready"; model: string; device: "cuda" | "cpu" | string; vramMb: number }
  | { state: "failed"; reason: string }
  /** First run: the speech runtime or model isn't installed yet (in-app setup fixes it). */
  | { state: "needsSetup"; reason: string }
  | { state: "unknown" };

/** One thing first-run speech setup installs (`runtime_setup::Part`). */
export interface SetupPart {
  id: string;
  label: string;
  bytes: number;
  done: boolean;
}

/** What setup would do on this computer (`runtime_setup::Plan`). */
export interface SetupPlan {
  supported: boolean;
  platform: "windows" | "macos" | "other";
  location: string;
  parts: SetupPart[];
  downloadBytes: number;
  neededBytes: number;
  freeBytes: number | null;
  logPath: string;
}

/** First-run speech setup (`runtime_setup::SetupStatus`, event `app://runtime-setup`). */
export type SetupStatus =
  | { state: "notNeeded" }
  | { state: "needed"; reason: string; plan: SetupPlan }
  | {
      state: "running";
      plan: SetupPlan;
      step: string;
      stepLabel: string;
      stepIndex: number;
      stepCount: number;
      fraction: number;
      detail: string;
    }
  | { state: "failed"; plan: SetupPlan; step: string; message: string; paused: boolean }
  | { state: "starting"; plan: SetupPlan }
  | { state: "done"; model: string; device: string };

/** Task 9 `watcher_status`: whether the correction watcher runs, and the probe's verdict. */
export interface WatcherStatus {
  enabled: boolean;
  verdict: string | null;
  reason: string | null;
}

/** App updater state from Rust (`updater::UpdateStatus`, emitted on `app://update-status`). */
export type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "upToDate"; checkedAt: string }
  | { state: "available"; version: string; notes?: string | null; pubDate?: string | null; releaseUrl?: string | null }
  | { state: "downloading"; version: string; downloadedBytes: number; totalBytes?: number | null; percent?: number | null }
  | { state: "installing"; version: string }
  | { state: "error"; message: string; version?: string | null };
