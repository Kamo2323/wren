// Made-up sample data for screenshots and for running the UI in a plain browser (no Tauri).
// Never real transcripts. British English, the sort of thing someone dictates to Claude while coding.

import type { DictionaryEntry, Stats, TranscriptionRow } from "./types";

const MIN = 60_000;

const SAMPLES: Array<{ ago: number; text: string; app: string; mode: "hold" | "handsFree"; secs: number }> = [
  {
    ago: 3,
    app: "claude.exe",
    mode: "hold",
    secs: 14,
    text:
      "Can you look at the settings loader and make it keep a backup when the file won't parse? At the moment it just falls back to the defaults and I lose everything.",
  },
  {
    ago: 11,
    app: "Code.exe",
    mode: "hold",
    secs: 6,
    text: "Rename this function to normaliseEngineStatus and add a short comment explaining why.",
  },
  {
    ago: 26,
    app: "claude.exe",
    mode: "handsFree",
    secs: 52,
    text:
      "Right, so the popup flickers when I press Control Windows and the right arrow together. I think the delay before we show it is too short. Could you check the hotkey timings against what Willow does, write a test for the tap case, and then tell me which constant you'd change before you change anything?",
  },
  {
    ago: 47,
    app: "OUTLOOK.EXE",
    mode: "hold",
    secs: 11,
    text: "Hi Sam, thanks for sending the drawings over. I'll have a proper look this afternoon and come back to you by Thursday at the latest.",
  },
  {
    ago: 95,
    app: "claude.exe",
    mode: "hold",
    secs: 9,
    text: "Run the tests again but only the store ones, and show me the raw output rather than a summary.",
  },
  {
    ago: 180,
    app: "WindowsTerminal.exe",
    mode: "hold",
    secs: 4,
    text: "git log, one line, last ten commits.",
  },
  {
    ago: 60 * 20,
    app: "claude.exe",
    mode: "handsFree",
    secs: 38,
    text:
      "Let's tidy up the stats page. I'd like the busiest hour to be obvious at a glance, and the thirty-day chart should start on the left with the oldest day. Keep the colours calm; it shouldn't look like a dashboard.",
  },
  {
    ago: 60 * 26,
    app: "msedge.exe",
    mode: "hold",
    secs: 7,
    text: "Best way to centre a div vertically in a flex container with a fixed height",
  },
];

export function mockTranscriptions(now = Date.now()): TranscriptionRow[] {
  return SAMPLES.map((s, i) => {
    const words = s.text.trim().split(/\s+/).length;
    return {
      id: `mock-${i + 1}`,
      createdAtUtc: new Date(now - s.ago * MIN).toISOString(),
      mode: s.mode,
      durationMs: s.secs * 1000,
      finalText: s.text,
      rawText: s.text,
      words,
      targetProcess: s.app,
      pasteOutcome: i === 3 ? "notPasted" : "pasted",
    };
  });
}

function isoDate(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** A believable month: busy weekdays, quiet weekends, a few days off, growing use. */
export function mockStats(now = new Date()): Stats {
  const pattern = [
    0, 0, 140, 320, 410, 0, 90, 520, 610, 380, 700, 0, 0, 450, 880, 960, 720, 1040, 160, 0, 1210, 980,
    1350, 1120, 870, 240, 610, 1480, 1260, 930,
  ];
  const daily = pattern.map((words, i) => {
    const d = new Date(now);
    d.setDate(d.getDate() - (29 - i));
    return { date: isoDate(d), words };
  });
  const hourly = [
    0, 0, 0, 0, 0, 0, 0, 120, 640, 1320, 1810, 1560, 880, 940, 1690, 2240, 1980, 1210, 520, 380, 610, 840,
    450, 90,
  ];
  return {
    timeSavedMsAll: 9 * 3600_000 + 42 * MIN,
    timeSavedMsWeek: 2 * 3600_000 + 18 * MIN,
    wpmToday: 148.6,
    wpmAll: 141.2,
    streakDays: 9,
    busiestHour: 15,
    wordsToday: 930,
    wordsWeek: 5870,
    wordsAll: 41_380,
    dictationsAll: 1_204,
    daily,
    hourly,
  };
}

export function emptyStats(now = new Date()): Stats {
  return {
    timeSavedMsAll: 0,
    timeSavedMsWeek: 0,
    wpmToday: null,
    wpmAll: null,
    streakDays: 0,
    busiestHour: null,
    wordsToday: 0,
    wordsWeek: 0,
    wordsAll: 0,
    dictationsAll: 0,
    daily: Array.from({ length: 30 }, (_, i) => {
      const d = new Date(now);
      d.setDate(d.getDate() - (29 - i));
      return { date: isoDate(d), words: 0 };
    }),
    hourly: new Array(24).fill(0),
  };
}

export function mockDictionary(): DictionaryEntry[] {
  const at = new Date().toISOString();
  return [
    { id: 1, kind: "term", fromText: null, toText: "Tauri", source: "manual", status: "active", observations: 0, hits: 31, createdAt: at },
    { id: 2, kind: "term", fromText: null, toText: "Overseer", source: "manual", status: "active", observations: 0, hits: 58, createdAt: at },
    { id: 3, kind: "replacement", fromText: "Klode", toText: "Claude", source: "learnedPaste", status: "active", observations: 2, hits: 17, createdAt: at },
    { id: 4, kind: "replacement", fromText: "rusqlight", toText: "rusqlite", source: "learnedHistory", status: "active", observations: 1, hits: 4, createdAt: at },
  ];
}
