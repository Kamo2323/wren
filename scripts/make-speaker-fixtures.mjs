// Writes the synthetic speaker-replay fixtures under src-tauri/tests/fixtures/speaker-replay/.
//
//   node scripts/make-speaker-fixtures.mjs
//
// Each fixture is a pretend Teams call: who spoke when (audio-clock ms), turned into what the Teams
// watcher would have read every 250 ms (the speaking highlight trails the audio by TEAMS_LAG_MS), plus
// the Them lines the segmenter would have cut (it only splits at 700 ms of silence, so fast
// back-and-forth lands in one line) and the answer key. Format: src-tauri/src/meetings/diagnostics.rs
// and replay.rs.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "tests", "fixtures", "speaker-replay");
const LAG = 900; // Teams lights the tile this long after the audio (speakers.rs TEAMS_LAG_MS)
const STEP = 250; // watcher poll
const ME = "Maximilian Haskell";

/**
 * @param {object} f
 * @param {string} f.name
 * @param {string[]} f.roster   other people, as Teams shows them
 * @param {[string, number, number][]} f.speech  [who ("me" or a roster name), startMs, endMs]
 * @param {{start:number,end:number,expected:string|null,also?:string[],text:string}[]} f.lines  Them lines
 * @param {[number, number][]} [f.blind]  reading-time ranges where Teams highlights nobody
 * @param {number[]} [f.flicker]  reading times where the highlight drops for one read
 * @param {number} f.endMs
 */
function fixture(f) {
  const out = [];
  out.push({ kind: "header", format: 1, meetingId: `synthetic-${f.name}`, startedAtUtc: "2026-11-26T10:00:00Z", teamsLagMs: LAG, platform: "synthetic" });
  const roster = [...f.roster, `${ME} (You)`];
  for (let at = 0; at < f.endMs; at += STEP) {
    const blind = (f.blind ?? []).some(([a, b]) => at >= a && at < b) || (f.flicker ?? []).includes(at);
    const speaking = blind
      ? []
      : f.speech.filter(([, s, e]) => at >= s + LAG && at < e + LAG).map(([who]) => (who === "me" ? ME : who));
    // Unmuted only around the user's own turns.
    const muted = !f.speech.some(([who, s, e]) => who === "me" && at >= s - 500 && at < e + 500);
    const r = { kind: "teams", atMs: at, speaking, selfMuted: muted };
    if (at === 0) Object.assign(r, { roster, selfName: ME, title: "Speaker test call" });
    out.push(r);
  }
  for (const [who, s, e] of f.speech.filter(([w]) => w === "me")) {
    out.push({ kind: "line", source: "me", startMs: s, endMs: e, speaker: ME, confidence: 1, muted: false, text: "(my line)" });
  }
  for (const l of f.lines) {
    // Synthetic: no live attribution was made, so the recorded speaker is left empty.
    out.push({ kind: "line", source: "them", startMs: l.start, endMs: l.end, speaker: null, confidence: 0, muted: false, text: l.text });
  }
  const dir = join(ROOT, f.name);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "call-diagnostics.jsonl"), out.map((o) => JSON.stringify(o)).join("\n") + "\n");
  const kase = {
    name: f.name,
    participants: f.roster,
    lines: f.lines.map((l) => ({
      startMs: l.start,
      endMs: l.end,
      text: l.text,
      expected: l.expected,
      ...(l.also ? { alsoSpeaking: l.also } : {}),
    })),
  };
  writeFileSync(join(dir, "case.json"), JSON.stringify(kase, null, 2) + "\n");
  console.log(`${f.name}: ${out.length} records, ${f.lines.length} Them lines`);
}

const SARAH = "Sarah Jones";
fixture({
  name: "one-person",
  roster: [SARAH],
  endMs: 38_000,
  speech: [
    [SARAH, 1_000, 4_500], ["me", 5_000, 6_500],
    [SARAH, 7_000, 12_000], ["me", 12_800, 14_500],
    [SARAH, 15_000, 16_200],
    [SARAH, 18_000, 24_000], ["me", 24_500, 25_500],
    [SARAH, 26_000, 27_500], ["me", 28_000, 29_500],
    [SARAH, 30_000, 36_000],
  ],
  flicker: [21_000],
  lines: [
    { start: 1_000, end: 4_500, expected: SARAH, text: "Morning, can you hear me okay?" },
    { start: 7_000, end: 12_000, expected: SARAH, text: "Great. So the plan for today is the launch checklist and the budget." },
    { start: 15_000, end: 16_200, expected: SARAH, text: "Sure, go ahead." },
    { start: 18_000, end: 24_000, expected: SARAH, text: "The checklist has twelve items and we have finished nine of them so far this week." },
    { start: 26_000, end: 27_500, expected: SARAH, text: "Yes, Thursday works." },
    { start: 30_000, end: 36_000, expected: SARAH, text: "Perfect, I will send the notes round after the call. Thanks everyone." },
  ],
});

const TOM = "Tom Reid";
const PRIYA = "Priya (Guest)";
const ALEX = "Alex (Guest)";
fixture({
  name: "five-person",
  roster: [TOM, SARAH, PRIYA, ALEX],
  endMs: 74_000,
  speech: [
    [TOM, 1_000, 5_000], ["me", 5_800, 8_000],
    [SARAH, 9_000, 13_000],
    [PRIYA, 14_000, 17_500],
    [ALEX, 18_500, 21_000],
    // Fast back-and-forth: gaps under 700 ms, so each group is one Them line.
    [TOM, 22_000, 23_200], [SARAH, 23_500, 24_600], [TOM, 24_900, 27_500],
    [SARAH, 28_500, 30_000], [PRIYA, 30_300, 33_500],
    [ALEX, 34_500, 35_500], [TOM, 35_800, 36_800], [ALEX, 37_100, 38_000],
    // Quick single turns with just enough silence to be separate lines.
    [SARAH, 39_000, 40_200], [TOM, 41_000, 42_300], [SARAH, 43_000, 44_100],
    // One overlap: Priya and Tom talk over each other for a second.
    [PRIYA, 45_000, 48_000], [TOM, 47_000, 49_500],
    // Teams shows nobody (screen share hides the tiles) while Alex talks.
    [ALEX, 51_000, 56_000],
    [SARAH, 58_000, 61_000], ["me", 61_500, 63_000],
    [TOM, 64_000, 68_000],
    [PRIYA, 69_000, 72_000],
  ],
  blind: [[50_500, 57_200]],
  flicker: [11_750],
  lines: [
    { start: 1_000, end: 5_000, expected: TOM, text: "Right, let's start. Everyone can see the agenda?" },
    { start: 9_000, end: 13_000, expected: SARAH, text: "Yes, and I added the supplier item at the bottom." },
    { start: 14_000, end: 17_500, expected: PRIYA, text: "I can take the supplier item, I spoke to them on Monday." },
    { start: 18_500, end: 21_000, expected: ALEX, text: "Can I go first? I have to drop at half past." },
    { start: 22_000, end: 27_500, expected: TOM, also: [SARAH], text: "Sure, go. Wait, which slide? The second one, the one with the dates on it." },
    { start: 28_500, end: 33_500, expected: PRIYA, also: [SARAH], text: "That one's old. No, I updated it last night, it's the March version now." },
    { start: 34_500, end: 38_000, expected: ALEX, also: [TOM], text: "Okay so. Go on Alex. So the numbers are up." },
    { start: 39_000, end: 40_200, expected: SARAH, text: "By how much?" },
    { start: 41_000, end: 42_300, expected: TOM, text: "Twelve percent." },
    { start: 43_000, end: 44_100, expected: SARAH, text: "That's brilliant." },
    { start: 45_000, end: 49_500, expected: PRIYA, also: [TOM], text: "And the supplier confirmed the delivery date as well, yes they did, last week." },
    { start: 51_000, end: 56_000, expected: ALEX, text: "I'm sharing my screen now, you should see the dashboard with the weekly figures." },
    { start: 58_000, end: 61_000, expected: SARAH, text: "Got it, the blue line is this year?" },
    { start: 64_000, end: 68_000, expected: TOM, text: "Yes, blue is this year and grey is last year." },
    { start: 69_000, end: 72_000, expected: PRIYA, text: "Lovely. I'll send the supplier contract round today." },
  ],
});
