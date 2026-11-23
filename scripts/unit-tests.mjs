// Unit tests for the pure UI helpers. Node 22.18+/23.6+ strips TypeScript types, so the .ts files load
// directly:  node scripts/unit-tests.mjs   (exit code 1 on any failure)
import assert from "node:assert/strict";
import { niceTicks, dayWpm } from "../src/lib/chart.ts";
import { replaceWord, wordCount, markMatches } from "../src/lib/text.ts";
import { copyTextWith } from "../src/lib/copy.ts";
import { formatBytes } from "../src/lib/format.ts";
import { dismissMsFor } from "../src/popup/dismiss.ts";
import { buildManifest, computeVersion, stampCargoVersion } from "./ci-release-meta.mjs";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

let failed = 0;
function test(name, fn) {
  try {
    fn();
    console.log(`PASS  ${name}`);
  } catch (e) {
    failed++;
    console.log(`FAIL  ${name}\n      ${e.message.split("\n").join("\n      ")}`);
  }
}

test("niceTicks(0) and bad input", () => {
  assert.deepEqual(niceTicks(0), [0]);
  assert.deepEqual(niceTicks(NaN), [0]);
});
test("niceTicks(1) has no duplicates", () => assert.deepEqual(niceTicks(1), [0, 1]));
test("niceTicks(7) is even", () => assert.deepEqual(niceTicks(7), [0, 5, 10]));
test("niceTicks(1480)", () => assert.deepEqual(niceTicks(1480), [0, 500, 1000, 1500]));
test("niceTicks(2240)", () => assert.deepEqual(niceTicks(2240), [0, 1000, 2000, 3000]));
test("niceTicks(160) for wpm", () => assert.deepEqual(niceTicks(160), [0, 100, 200]));
test("niceTicks: unique, whole, ascending, covers max", () => {
  for (const max of [1, 2, 3, 5, 7, 9, 11, 23, 48, 99, 101, 640, 999, 1001, 12345]) {
    const t = niceTicks(max);
    assert.equal(new Set(t).size, t.length, `duplicates for ${max}: ${t}`);
    assert.ok(t.every(Number.isInteger), `non-integer for ${max}: ${t}`);
    assert.ok(t.at(-1) >= max, `top below max for ${max}: ${t}`);
    const steps = t.slice(1).map((v, i) => v - t[i]);
    assert.ok(steps.every((s) => s === steps[0]), `uneven for ${max}: ${t}`);
  }
});
test("dayWpm: gap when nothing spoken", () => {
  assert.equal(dayWpm(0, 0), null);
  assert.equal(dayWpm(120, 0), null);
  assert.equal(dayWpm(150, 60_000), 150);
});
test("replaceWord mirrors Rust replace_whole_word", () => {
  // the same cases as src-tauri/src/dictionary/mod.rs
  assert.equal(replaceWord("Ask Klode, then klode again.", "Klode", "Claude"), "Ask Claude, then Claude again.");
  assert.equal(replaceWord("cash is king. Cash flow", "cash", "cache"), "cache is king. Cache flow");
  assert.equal(replaceWord("cashew cash cash's", "cash", "cache"), "cashew cache cash's");
  assert.equal(replaceWord("pie test, pie testing", "pie test", "pytest"), "pytest, pie testing");
  assert.equal(replaceWord("café café", "café", "cafe"), "cafe cafe");
  assert.equal(replaceWord("nothing here", "klode", "Claude"), "nothing here");
  assert.equal(replaceWord("x", "", "y"), "x");
});
test("wordCount", () => {
  assert.equal(wordCount("  two   words "), 2);
  assert.equal(wordCount(""), 0);
});
test("markMatches", () => {
  assert.deepEqual(markMatches("A cache, Cache", "cache"), [
    { text: "A ", match: false },
    { text: "cache", match: true },
    { text: ", ", match: false },
    { text: "Cache", match: true },
  ]);
  assert.deepEqual(markMatches("a.b", "."), [
    { text: "a", match: false },
    { text: ".", match: true },
    { text: "b", match: false },
  ]);
});

async function testAsync(name, fn) {
  try {
    await fn();
    console.log(`PASS  ${name}`);
  } catch (e) {
    failed++;
    console.log(`FAIL  ${name}\n      ${e.message.split("\n").join("\n      ")}`);
  }
}

// I-2: dictated text is copied by Rust (excluded from Win+V history and cloud clipboard), never by
// the WebView's clipboard API, whenever the page runs inside Tauri.
await testAsync("copyTextWith: in Tauri, Rust copies and the browser clipboard is never used", async () => {
  const calls = [];
  const ok = await copyTextWith("Dictated words", {
    inTauri: true,
    rustCopy: async (t) => calls.push(["rust", t]),
    browserCopy: async (t) => (calls.push(["browser", t]), true),
  });
  assert.equal(ok, true);
  assert.deepEqual(calls, [["rust", "Dictated words"]]);
});
await testAsync("copyTextWith: in Tauri, a Rust failure is reported, with no browser fallback", async () => {
  const calls = [];
  const ok = await copyTextWith("", {
    inTauri: true,
    rustCopy: async () => {
      calls.push("rust");
      throw new Error("nothing to copy");
    },
    browserCopy: async () => (calls.push("browser"), true),
  });
  assert.equal(ok, false);
  assert.deepEqual(calls, ["rust"]);
});
await testAsync("copyTextWith: in a plain browser (mock), the browser clipboard is used", async () => {
  const calls = [];
  const ok = await copyTextWith("x", {
    inTauri: false,
    rustCopy: async () => calls.push("rust"),
    browserCopy: async (t) => (calls.push(["browser", t]), true),
  });
  assert.equal(ok, true);
  assert.deepEqual(calls, [["browser", "x"]]);
});
test("no page copies with navigator.clipboard / execCommand except copy.ts's browser-only path", () => {
  const root = fileURLToPath(new URL("../src/", import.meta.url));
  const offenders = [];
  const walk = (dir) => {
    for (const name of readdirSync(dir)) {
      const p = join(dir, name);
      if (statSync(p).isDirectory()) walk(p);
      else if (/\.(ts|tsx)$/.test(name)) {
        const rel = relative(root, p).replaceAll("\\", "/");
        const src = readFileSync(p, "utf8");
        if (rel !== "lib/copy.ts" && /navigator\.clipboard|execCommand\(/.test(src)) offenders.push(rel);
      }
    }
  };
  walk(root);
  assert.deepEqual(offenders, []);
});

// M-1: "No sound" shown during a recording never dismisses itself (the hands-free × must stay).
test("dismissMsFor: noSound while recording stays; after a recording it dismisses", () => {
  const d = { pasted: 1500, noSound: 4000, notPasted: 6000 };
  assert.equal(dismissMsFor({ kind: "noSound", recording: true }, d), undefined);
  assert.equal(dismissMsFor({ kind: "noSound", recording: false }, d), 4000);
  assert.equal(dismissMsFor({ kind: "pasted" }, d), 1500);
  assert.equal(dismissMsFor({ kind: "handsfree", startedAtMs: 1, cpu: false }, d), undefined);
});

test("formatBytes formats B, KB, and MB cleanly", () => {
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(512 * 1024), "512 KB");
  assert.equal(formatBytes(10 * 1024 * 1024), "10.0 MB");
});

test("ci-release-meta computes versions, stamps Cargo.toml, and builds latest.json", () => {
  assert.equal(computeVersion("0.1.0", { runNumber: "7", explicitVersion: "" }), "0.1.7");
  assert.equal(computeVersion("0.1.0", { runNumber: "7", explicitVersion: "v0.2.0" }), "0.2.0");
  assert.throws(() => computeVersion("0.1.0", { runNumber: "0", explicitVersion: "" }));
  const cargo = '[package]\nname = "wren"\nversion = "0.1.0"\n\n[dependencies]\nserde = { version = "1" }\n';
  const stamped = stampCargoVersion(cargo, "0.1.7");
  assert.ok(stamped.includes('version = "0.1.7"'));
  assert.ok(stamped.includes('serde = { version = "1" }'), "dependency versions stay untouched");
  const manifest = buildManifest({
    version: "v0.1.7",
    repo: "Kamo2323/wren",
    installer: "Wren_0.1.7_x64-setup.exe",
    signature: "  SIGDATA  \n",
    notes: "  - Fix bug  ",
    pubDate: "2026-11-23T03:00:00Z",
  });
  assert.equal(manifest.version, "0.1.7");
  assert.equal(manifest.notes, "- Fix bug");
  assert.equal(manifest.platforms["windows-x86_64"].signature, "SIGDATA");
  assert.equal(
    manifest.platforms["windows-x86_64"].url,
    "https://github.com/Kamo2323/wren/releases/download/v0.1.7/Wren_0.1.7_x64-setup.exe",
  );
});

console.log(failed ? `${failed} test(s) failed` : "all unit tests passed");
process.exit(failed ? 1 : 0);
