// Typed wrappers over Tauri `invoke()` for every command the UI uses (Tasks 1, 4, 8, 9).
// When the page is not running inside Tauri (Playwright screenshots, `vite` in a browser) every call
// falls back to mock data from mock.ts, so the UI can be built and checked without the Rust side.
// Add `?mock=empty` to the URL for the first-run (no data) look. Other mock switches (screenshots only):
// `?others=Willow,Glaido` (apps running), `?ai=offline|nomodel` (AI clean-up status), `?watcher=off`,
// `?big=1` (six-figure totals), `?fail=1` (every read fails, for the error states), `?engine=failed|cpu`.

import type {
  AiCleanupStatus,
  DictionaryEntry,
  EngineStatus,
  EntryStatus,
  FixWordResult,
  PopupState,
  Settings,
  Stats,
  TranscriptionRow,
  UiConstants,
  WatcherStatus,
} from "./types";
import { emptyStats, mockDictionary, mockStats, mockTranscriptions } from "./mock";
import { replaceWord, wordCount } from "./text";

export { replaceWord } from "./text";

export const inTauri: boolean =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in (window as unknown as Record<string, unknown>);

const mockParams = typeof location !== "undefined" ? new URLSearchParams(location.search) : new URLSearchParams();
const mockEmpty = mockParams.get("mock") === "empty";

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

// ---------- mock store (browser only) ----------
// Every mock read hands out copies, like the real IPC does, so the pages can't share objects with the store.

const mock = {
  history: mockEmpty ? [] : mockTranscriptions(),
  dictionary: mockEmpty ? [] : mockDictionary(),
  nextId: 100,
  aiModelPresent: mockParams.get("ai") !== "nomodel",
  settings: {
    version: 1,
    engine: {
      model: "large-v3-turbo",
      computeType: "int8_float16",
      beamSize: 1,
      cpuFallbackModel: "small.en",
      language: "en",
      vadFilter: true,
      conditionOnPreviousText: false,
    },
    micDevice: null,
    alwaysOpenMic: false,
    typingWpm: 52,
    startWithWindows: false,
    sounds: false,
    aiCleanup: { enabled: false, model: "qwen2.5:3b" },
    watcherEnabled: null,
    willowSeedImported: false,
  } as Settings,
};

const later = <T,>(value: T, ms = 60) => new Promise<T>((r) => setTimeout(() => r(value), ms));
const mockFail = mockParams.get("fail") === "1";
/** A mock read: like `later`, but rejects under `?fail=1` so the error states can be seen. */
const read = <T,>(value: T) => (mockFail ? new Promise<T>((_, no) => setTimeout(() => no(new Error("mock failure")), 60)) : later(value));

// ---------- Task 1: app + settings ----------

/** Defaults matching src-tauri/src/constants.rs; used before `get_constants` answers and in mock mode. */
export const DEFAULT_CONSTANTS: UiConstants = {
  retentionHours: 72,
  typingWpmDefault: 52,
  popupWindowW: 360,
  popupWindowH: 120,
  recordingWarnMs: 540_000,
  maxRecordingMs: 600_000,
  popupPastedMs: 400,
  popupStillWorkingAfterMs: 1500,
  popupNothingMs: 1500,
  popupNoSoundMs: 4000,
  popupNotPastedMs: 4000,
  popupLearnedMs: 6000,
  popupErrorMs: 5000,
};

export const getConstants = (): Promise<UiConstants> =>
  inTauri ? call<UiConstants>("get_constants").then((c) => ({ ...DEFAULT_CONSTANTS, ...c })) : later({ ...DEFAULT_CONSTANTS });

export const appVersion = (): Promise<string> => (inTauri ? call("app_version") : later("0.1.0"));
export const getSettings = (): Promise<Settings> => (inTauri ? call("get_settings") : read(structuredClone(mock.settings)));
export const saveSettings = (settings: Settings): Promise<void> => {
  if (inTauri) return call("save_settings", { settings });
  mock.settings = structuredClone(settings);
  return later(undefined);
};

// ---------- Task 4: history, stats, dictionary ----------

export const historyRecent = (limit: number): Promise<TranscriptionRow[]> =>
  inTauri ? call("history_recent", { limit }) : read(structuredClone(mock.history.slice(0, limit)));

export const historySearch = (query: string): Promise<TranscriptionRow[]> => {
  if (inTauri) return call("history_search", { query });
  const q = query.trim().toLowerCase();
  return read(structuredClone(mock.history.filter((r) => r.finalText.toLowerCase().includes(q))));
};

export const historyDelete = (id: string): Promise<void> => {
  if (inTauri) return call("history_delete", { id });
  mock.history = mock.history.filter((r) => r.id !== id);
  return later(undefined);
};

/** Deletes every transcription (and its audio) now; resolves to how many were deleted. Stats stay. */
export const historyDeleteAll = (): Promise<number> => {
  if (inTauri) return call("history_delete_all");
  const n = mock.history.length;
  mock.history = [];
  return later(n);
};

export const statsGet = (): Promise<Stats> =>
  inTauri ? call("stats_get") : read(mockEmpty ? emptyStats() : mockStats(new Date(), mockParams.get("big") === "1"));

/** Replace `from` with `to` in a transcription and teach the dictionary (Task 4 `fix_word`). */
export const historyFixWord = (id: string, from: string, to: string): Promise<FixWordResult> => {
  if (inTauri) return call("history_fix_word", { id, from, to });
  const row = mock.history.find((r) => r.id === id);
  if (row) {
    // mirrors Rust Store::fix_word: the text is rewritten and its word count follows
    row.finalText = replaceWord(row.finalText, from, to);
    row.words = wordCount(row.finalText);
  }
  const entry: DictionaryEntry = {
    id: mock.nextId++,
    kind: "replacement",
    fromText: from,
    toText: to,
    source: "learnedHistory",
    status: "active",
    observations: 1,
    hits: 0,
    createdAt: new Date().toISOString(),
  };
  mock.dictionary.push(entry);
  let term: DictionaryEntry | null = null;
  if (!mock.dictionary.some((d) => d.kind === "term" && d.toText.toLowerCase() === to.toLowerCase())) {
    term = { ...entry, id: mock.nextId++, kind: "term", fromText: null };
    mock.dictionary.push(term);
  }
  return later(structuredClone({ replacement: entry, term }));
};

export const dictionaryList = (): Promise<DictionaryEntry[]> => (inTauri ? call("dictionary_list") : read(structuredClone(mock.dictionary)));

export const dictionaryAddTerm = (word: string): Promise<DictionaryEntry> => {
  if (inTauri) return call("dictionary_add_term", { word });
  const e: DictionaryEntry = { id: mock.nextId++, kind: "term", fromText: null, toText: word, source: "manual", status: "active", observations: 0, hits: 0, createdAt: new Date().toISOString() };
  mock.dictionary.push(e);
  return later(structuredClone(e));
};

export const dictionaryAddReplacement = (from: string, to: string): Promise<DictionaryEntry> => {
  if (inTauri) return call("dictionary_add_replacement", { from, to });
  const e: DictionaryEntry = { id: mock.nextId++, kind: "replacement", fromText: from, toText: to, source: "manual", status: "active", observations: 0, hits: 0, createdAt: new Date().toISOString() };
  mock.dictionary.push(e);
  return later(structuredClone(e));
};

export const dictionarySetStatus = (id: number, status: EntryStatus): Promise<void> => {
  if (inTauri) return call("dictionary_set_status", { id, status });
  const e = mock.dictionary.find((d) => d.id === id);
  if (e) e.status = status;
  return later(undefined);
};

export const dictionaryDelete = (id: number): Promise<void> => {
  if (inTauri) return call("dictionary_delete", { id });
  mock.dictionary = mock.dictionary.filter((d) => d.id !== id);
  return later(undefined);
};

// ---------- Task 8: popup actions, engine, audio ----------

export const popupCopyText = (text: string): Promise<void> => (inTauri ? call("popup_copy_text", { text }) : later(undefined));
export const popupUndoLearned = (entryId: number): Promise<void> => (inTauri ? call("popup_undo_learned", { entryId }) : later(undefined));
export const popupDiscard = (): Promise<void> => (inTauri ? call("popup_discard") : later(undefined));
export const popupOpenMain = (): Promise<void> => (inTauri ? call("popup_open_main") : later(undefined));
/** "No sound from your mic" click: Windows' microphone privacy settings. */
export const popupOpenMicSettings = (): Promise<void> => (inTauri ? call("popup_open_mic_settings") : later(undefined));
/** Where the pill sits inside the popup window (CSS px), so only the pill catches clicks. */
export const popupHitRect = (r: { x: number; y: number; w: number; h: number }): Promise<void> =>
  inTauri ? call("popup_hit_rect", r) : Promise.resolve();

let readySent = false;
/** The main window has painted its first screen: Rust shows the window (once). Safe to call often. */
export function mainReady(): void {
  if (readySent || !inTauri) return;
  readySent = true;
  void call("main_ready").catch(() => {});
}

/** Accepts the Rust `EngineStatus` however serde tags it (unit strings, external or internal tags). */
export function normaliseEngineStatus(raw: unknown): EngineStatus {
  if (raw == null) return { state: "unknown" };
  if (typeof raw === "string") {
    const s = raw.toLowerCase();
    if (s === "starting") return { state: "starting" };
    if (s === "warmingup" || s === "warming_up" || s === "warming") return { state: "warming" };
    return { state: "unknown" };
  }
  if (typeof raw !== "object") return { state: "unknown" };
  const o = raw as Record<string, unknown>;
  const tag = (o.kind ?? o.state ?? o.status ?? o.type) as string | undefined;
  if (typeof tag === "string") {
    const inner = normaliseEngineStatus(tag);
    if (inner.state !== "unknown") return inner;
    const t = tag.toLowerCase();
    if (t === "ready") return readyFrom(o);
    if (t === "failed" || t === "error") return { state: "failed", reason: String(o.reason ?? o.message ?? "") };
  }
  if ("Ready" in o || "ready" in o) return readyFrom((o.Ready ?? o.ready) as Record<string, unknown>);
  if ("Failed" in o || "failed" in o) {
    const f = (o.Failed ?? o.failed) as Record<string, unknown> | string;
    return { state: "failed", reason: typeof f === "string" ? f : String(f?.reason ?? "") };
  }
  return { state: "unknown" };
}

function readyFrom(o: Record<string, unknown>): EngineStatus {
  return {
    state: "ready",
    model: String(o?.model ?? ""),
    device: String(o?.device ?? "cuda"),
    vramMb: Number(o?.vramMb ?? o?.vram_mb ?? 0),
  };
}

export const engineStatus = async (): Promise<EngineStatus> =>
  inTauri ? normaliseEngineStatus(await call<unknown>("engine_status")) : later(mockEngine());
/** `?engine=failed|cpu` (screenshots and checks only). */
function mockEngine(): EngineStatus {
  const e = mockParams.get("engine");
  if (e === "failed") return { state: "failed", reason: "The speech engine stopped 5 times in 5 minutes." };
  return { state: "ready", model: "large-v3-turbo", device: e === "cpu" ? "cpu" : "cuda", vramMb: e === "cpu" ? 0 : 1840 };
}
export const engineRestart = (): Promise<void> => (inTauri ? call("engine_restart") : later(undefined));
export const audioDevices = (): Promise<string[]> =>
  inTauri ? call("audio_devices") : later(["Microphone (Shure MV7)", "Headset Microphone (Jabra Evolve2 65)", "Microphone Array (Realtek)"]);
/** `null` = follow the Windows default microphone. */
export const setAudioDevice = (name: string | null): Promise<void> => {
  if (inTauri) return call("set_audio_device", { name });
  mock.settings.micDevice = name;
  return later(undefined);
};
/** Names of other dictation apps running now (e.g. ["Glaido"]); empty when none. */
export const otherAppsRunning = (): Promise<string[]> => {
  if (inTauri) return call("other_apps_running");
  const others = mockParams.get("others");
  return later(others ? others.split(",").filter(Boolean) : []);
};

// ---------- Task 9: correction watcher, AI clean-up ----------

export const watcherStatus = (): Promise<WatcherStatus> => {
  if (inTauri) return call("watcher_status");
  const off = mockParams.get("watcher") === "off";
  return later(
    off
      ? { enabled: false, verdict: "slow", reason: "The Claude app's text box took 180 ms to read, so watching it could slow typing." }
      : { enabled: true, verdict: "readable", reason: "The Claude app's text box can be read in 12 ms." },
  );
};
export const aiCleanupStatus = (): Promise<AiCleanupStatus> => {
  if (inTauri) return call("ai_cleanup_status");
  const model = mock.settings.aiCleanup.model;
  if (mockParams.get("ai") === "offline") return later({ ollamaReachable: false, modelPresent: false, model });
  return later({ ollamaReachable: true, modelPresent: mock.aiModelPresent, model });
};
/** Downloads the clean-up model through Ollama; resolves when the download has finished. */
export const aiCleanupPullModel = (): Promise<void> => {
  if (inTauri) return call("ai_cleanup_pull_model");
  mock.aiModelPresent = true;
  return later(undefined, 2500);
};

// ---------- events ----------

type Unlisten = () => void;

async function on<T>(event: string, handler: (payload: T) => void): Promise<Unlisten> {
  if (!inTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<T>(event, (e) => handler(e.payload));
}

/**
 * Subscribe from a React effect: returns a synchronous cleanup that also unsubscribes when `listen`
 * resolves after the component has already gone (no leaked listeners).
 */
export function subscribe(start: Promise<Unlisten> | Array<Promise<Unlisten>>): Unlisten {
  let cancelled = false;
  const offs: Unlisten[] = [];
  for (const p of Array.isArray(start) ? start : [start]) {
    void p.then((u) => {
      if (cancelled) u();
      else offs.push(u);
    });
  }
  return () => {
    cancelled = true;
    offs.splice(0).forEach((u) => u());
  };
}

export const onPopupState = (h: (s: PopupState) => void) => on<PopupState>("popup://state", h);
export const onPopupLevel = (h: (level: number) => void) => on<number>("popup://level", h);
export const onEngineStatus = (h: (s: EngineStatus) => void) => on<unknown>("app://engine-status", (raw) => h(normaliseEngineStatus(raw)));
export const onHistoryChanged = (h: () => void) => on<unknown>("app://history-changed", () => h());

// ---------- helpers ----------

/** Copy text from the main window. Falls back to a hidden textarea if the async API is blocked. */
export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.position = "fixed";
    ta.style.opacity = "0";
    document.body.appendChild(ta);
    ta.select();
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch {
      ok = false;
    }
    ta.remove();
    return ok;
  }
}
