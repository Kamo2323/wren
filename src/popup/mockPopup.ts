// Popup preview without Tauri: popup.html?state=<kind>[&hold=1][&silent=1][&cpu=1][&slow=1][&hf=1]
// (hf=1: the cap warning arrives during hands-free)
// `state=demo` walks through every state on a loop. Levels come from a speech-like generator.
import type { PopupKind, PopupState } from "../lib/types";
import { levelBus } from "./LevelBars";

export function samplePopupState(kind: PopupKind, opts: { cpu?: boolean; slow?: boolean; handsFree?: boolean } = {}): PopupState {
  const now = Date.now();
  switch (kind) {
    case "listening":
      return { kind, cpu: !!opts.cpu };
    case "handsfree":
      return { kind, startedAtMs: now - 42_000, cpu: !!opts.cpu };
    case "transcribing":
      return { kind, sinceMs: opts.slow ? now - 2_000 : now };
    case "notPasted":
      return { kind, text: "Can you look at the settings loader and make it keep a backup?" };
    case "learned":
      return { kind, from: "Klode", to: "Claude", entryId: 3 };
    case "error":
      return { kind, message: "Microphone unplugged" };
    case "capWarning":
      return { kind, remainingMs: 60_000, mode: opts.handsFree ? "handsFree" : "hold" };
    case "noSound":
      return { kind, recording: false };
    default:
      return { kind } as PopupState;
  }
}

/** Speech-like level: syllables (~4 Hz) inside words (~1 Hz phrases), with short pauses. */
export function speechLevel(t: number): number {
  const phrase = Math.max(0, Math.sin(2 * Math.PI * 0.42 * t) * 0.7 + 0.45);
  const syll = 0.55 + 0.45 * Math.abs(Math.sin(2 * Math.PI * 2.1 * t + Math.sin(t * 1.3)));
  const grit = 0.9 + 0.1 * Math.sin(2 * Math.PI * 11 * t);
  return Math.min(1, phrase * syll * grit * 0.95);
}

/** Push mock levels at 30 Hz. Returns a stop function. */
export function startMockLevels(): () => void {
  const t0 = performance.now();
  const id = window.setInterval(() => levelBus.push(speechLevel((performance.now() - t0) / 1000 + 1.3)), 33);
  return () => window.clearInterval(id);
}

export const DEMO_SEQUENCE: Array<{ kind: PopupKind; ms: number }> = [
  { kind: "listening", ms: 3200 },
  { kind: "transcribing", ms: 1100 },
  { kind: "pasted", ms: 900 },
  { kind: "hidden", ms: 700 },
  { kind: "handsfree", ms: 4200 },
  { kind: "transcribing", ms: 2400 },
  { kind: "learned", ms: 3000 },
  { kind: "hidden", ms: 700 },
  { kind: "warming", ms: 1800 },
  { kind: "nothing", ms: 1700 },
  { kind: "hidden", ms: 600 },
  { kind: "notPasted", ms: 2600 },
  { kind: "error", ms: 2600 },
  { kind: "hidden", ms: 900 },
];
