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
  {
    ago: 60 * 29,
    app: "slack.exe",
    mode: "hold",
    secs: 8,
    text: "Morning all, I'm working from home today. Ping me if the build breaks and I'll take a look straight away.",
  },
  {
    ago: 60 * 33,
    app: "claude.exe",
    mode: "hold",
    secs: 17,
    text:
      "Add a search box to the history page. It should search everything from the last three days and highlight the words that match, and pressing Escape should clear it.",
  },
  {
    ago: 60 * 50,
    app: "WINWORD.EXE",
    mode: "handsFree",
    secs: 44,
    text:
      "The second phase covers the warehouse in Leeds. We expect the fit-out to take about six weeks, with the racking going in first and the cold store after that. Siobhan will confirm the dates once the drawings are signed off.",
  },
  {
    ago: 60 * 55,
    app: "claude.exe",
    mode: "hold",
    secs: 6,
    text: "Why is the cache not being cleared when the settings change?",
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
export function mockStats(now = new Date(), big = false): Stats {
  const pattern = [
    0, 0, 140, 320, 410, 0, 90, 520, 610, 380, 700, 0, 0, 450, 880, 960, 720, 1040, 160, 0, 1210, 980,
    1350, 1120, 870, 240, 610, 1480, 1260, 930,
  ];
  // speaking speed drifts up over the month (≈128 → 150 wpm) with a little day-to-day wobble
  const wobble = [3, -4, 6, -2, 1, -5, 4, 0, -3, 5, 2, -6, 3, 1, -2, 6, -4, 2, 0, -3, 5, -1, 3, -5, 4, 1, -2, 6, -3, 2];
  const daily = pattern.map((words, i) => {
    const d = new Date(now);
    d.setDate(d.getDate() - (29 - i));
    const wpm = 128 + (i * 20) / 29 + wobble[i];
    return { date: isoDate(d), words, speakingMs: words > 0 ? Math.round((words / wpm) * 60_000) : 0 };
  });
  const hourly = [
    0, 0, 0, 0, 0, 0, 0, 120, 640, 1320, 1810, 1560, 880, 940, 1690, 2240, 1980, 1210, 520, 380, 610, 840,
    450, 90,
  ];
  if (big) {
    // six-figure totals, for checking the tiles don't overflow at 960 px
    return {
      timeSavedMsAll: 31 * 3600_000 + 7 * MIN,
      timeSavedMsWeek: 4 * 3600_000 + 51 * MIN,
      wpmToday: 152.3,
      wpmAll: 144.9,
      streakDays: 128,
      busiestHour: 15,
      wordsToday: 12_345,
      wordsWeek: 48_210,
      wordsAll: 123_456,
      dictationsAll: 9_876,
      daily,
      hourly,
    };
  }
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
      return { date: isoDate(d), words: 0, speakingMs: 0 };
    }),
    hourly: new Array(24).fill(0),
  };
}

export function mockDictionary(now = Date.now()): DictionaryEntry[] {
  const ago = (days: number) => new Date(now - days * 86_400_000).toISOString();
  let id = 1;
  const term = (word: string, source: DictionaryEntry["source"], hits: number, days: number, status: DictionaryEntry["status"] = "active"): DictionaryEntry => ({
    id: id++, kind: "term", fromText: null, toText: word, source, status, observations: 0, hits, createdAt: ago(days),
  });
  const rep = (
    from: string,
    to: string,
    source: DictionaryEntry["source"],
    hits: number,
    days: number,
    status: DictionaryEntry["status"] = "active",
    observations = 1,
  ): DictionaryEntry => ({ id: id++, kind: "replacement", fromText: from, toText: to, source, status, observations, hits, createdAt: ago(days) });
  return [
    // words added by hand
    term("Tauri", "manual", 31, 21),
    term("WebView2", "manual", 12, 20),
    term("Playwright", "manual", 9, 18),
    term("Siobhan", "manual", 6, 14),
    term("Kubernetes", "manual", 4, 12),
    term("Ollama", "manual", 3, 9),
    term("Cloudflare", "manual", 14, 7),
    term("rubato", "manual", 0, 1),
    // replacements added by hand
    rep("web view two", "WebView2", "manual", 5, 20, "active", 0),
    rep("see sharp", "C#", "manual", 2, 16, "active", 0),
    rep("pie test", "pytest", "manual", 7, 11, "active", 0),
    // learned from edits in the text box
    rep("Klode", "Claude", "learnedPaste", 17, 6),
    term("Claude", "learnedPaste", 23, 6),
    rep("cash", "cache", "learnedPaste", 0, 1, "candidate", 1),
    rep("rust sequel", "rusqlite", "learnedPaste", 3, 3),
    term("rusqlite", "learnedPaste", 3, 3),
    // learned from "Fix a word" in History
    rep("Shivon", "Siobhan", "learnedHistory", 4, 2),
    rep("tory", "Tauri", "learnedHistory", 9, 4),
    // the user said no to this one
    rep("affect", "effect", "learnedPaste", 0, 5, "disabled", 2),
  ];
}
