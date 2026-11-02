// Unit tests for the pure UI helpers. Node 22.18+/23.6+ strips TypeScript types, so the .ts files load
// directly:  node scripts/unit-tests.mjs   (exit code 1 on any failure)
import assert from "node:assert/strict";
import { niceTicks, dayWpm } from "../src/lib/chart.ts";
import { replaceWord, wordCount, markMatches } from "../src/lib/text.ts";

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

console.log(failed ? `${failed} test(s) failed` : "all unit tests passed");
process.exit(failed ? 1 : 0);
