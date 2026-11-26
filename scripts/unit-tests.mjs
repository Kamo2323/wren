// Unit tests for the pure UI helpers. Node 22.18+/23.6+ strips TypeScript types, so the .ts files load
// directly:  node scripts/unit-tests.mjs   (exit code 1 on any failure)
import assert from "node:assert/strict";
import { niceTicks, dayWpm } from "../src/lib/chart.ts";
import { replaceWord, wordCount, markMatches } from "../src/lib/text.ts";
import { copyTextWith } from "../src/lib/copy.ts";
import { appName, computeTimeSavedMs, formatBytes } from "../src/lib/format.ts";
import { normalizeThemePreference, resolveTheme, toSettingsTheme } from "../src/lib/theme.ts";
import { dismissMsFor } from "../src/popup/dismiss.ts";
import { assignColours, speakerColour, groupRuns, formatElapsed, displayName, captureNote, footerLabel, spendLine } from "../src/lib/meetingText.ts";
import { buildManifest, computeVersion, stampCargoVersion } from "./ci-release-meta.mjs";
import { emailHtml, pdfDocument, plainText, buildDocx, exportFileName } from "../src/lib/notesExport.ts";
import JSZip from "jszip";
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

test("theme helpers resolve dark, light and system preferences", () => {
  assert.equal(normalizeThemePreference("dark"), "dark");
  assert.equal(normalizeThemePreference("light"), "light");
  assert.equal(normalizeThemePreference(null), "system");
  assert.equal(normalizeThemePreference(undefined), "system");
  assert.equal(toSettingsTheme("system"), null);
  assert.equal(toSettingsTheme("dark"), "dark");
  assert.equal(resolveTheme("dark", false), "dark");
  assert.equal(resolveTheme("light", true), "light");
  assert.equal(resolveTheme("system", true), "dark");
  assert.equal(resolveTheme("system", false), "light");
});

test("computeTimeSavedMs and appName match Rust behaviour", () => {
  assert.equal(computeTimeSavedMs(52, 0, 52), 60_000);
  assert.equal(computeTimeSavedMs(10, 600_000, 52), 0);
  assert.equal(computeTimeSavedMs(52, 0, 0), 60_000);
  assert.equal(appName("T3 Code (Beta).exe"), "T3 Code");
  assert.equal(appName("T3 Code (Beta)"), "T3 Code");
  assert.equal(appName("claude.exe"), "Claude");
  assert.equal(appName("Google Chrome"), "Chrome");
  assert.equal(appName("Safari.app"), "Safari");
});

test("ci-release-meta computes versions, stamps Cargo.toml, and builds multi-platform latest.json", () => {
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
    macInstaller: "Wren_0.1.7_aarch64.app.tar.gz",
    macSignature: "  MACSIG  \n",
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
  assert.equal(manifest.platforms["darwin-aarch64"].signature, "MACSIG");
  assert.equal(
    manifest.platforms["darwin-aarch64"].url,
    "https://github.com/Kamo2323/wren/releases/download/v0.1.7/Wren_0.1.7_aarch64.app.tar.gz",
  );
});

// ---------- meetings (plan Task 11) ----------
const seg = (id, source, speaker, startMs, text, extra = {}) => ({
  id, meetingId: "m", source, speaker, confidence: 0.9, renamed: false, muted: false, startMs, endMs: startMs + 2000, text, ...extra,
});
test("speakerColour is stable and in range", () => {
  assert.equal(speakerColour("Sarah Jones"), speakerColour("Sarah Jones"));
  for (const n of ["a", "Tom Reid", "Speaker", ""]) {
    const c = speakerColour(n);
    assert.ok(c >= 0 && c < 6, `${n} → ${c}`);
  }
  assert.notEqual(new Set(["Sarah Jones", "Tom Reid", "Ana", "Bo", "Cy", "Di"].map(speakerColour)).size, 1);
});
test("groupRuns merges consecutive lines of one speaker", () => {
  const runs = groupRuns([
    seg(1, "them", "Sarah", 0, "a"),
    seg(2, "them", "Sarah", 3000, "b"),
    seg(3, "me", "Max", 6000, "c"),
    seg(4, "them", "Sarah", 9000, "d"),
    seg(5, "them", "Sarah", 200000, "e"),
  ]);
  assert.deepEqual(runs.map((r) => r.lines.map((l) => l.id)), [[1, 2], [3], [4], [5]], "a long gap starts a new run");
});
test("groupRuns splits a very long monologue so the transcript stays windowed", () => {
  const many = Array.from({ length: 95 }, (_, i) => seg(i + 1, "them", "Sarah", i * 3000, "x"));
  const runs = groupRuns(many);
  assert.ok(runs.length >= 3, `${runs.length}`);
  assert.ok(runs.every((r) => r.lines.length <= 40));
});
test("groupRuns keeps unnamed Them lines apart from named ones", () => {
  const runs = groupRuns([seg(1, "them", null, 0, "a"), seg(2, "them", "Tom", 1000, "b"), seg(3, "them", null, 2000, "c")]);
  assert.equal(runs.length, 3);
});
test("assignColours gives distinct colours in speaking order, Me first", () => {
  const m = assignColours(["Sarah Jones", "Maximilian", "Tom Reid", "Sarah Jones", "Ana"], "Maximilian");
  assert.equal(m.get("Maximilian"), 0);
  assert.equal(m.get("Sarah Jones"), 1);
  assert.equal(m.get("Tom Reid"), 2);
  assert.equal(m.get("Ana"), 3);
  assert.equal(new Set(m.values()).size, 4);
});
test("formatElapsed", () => {
  assert.equal(formatElapsed(0), "00:00");
  assert.equal(formatElapsed(65_400), "01:05");
  assert.equal(formatElapsed(3_725_000), "1:02:05");
});
test("displayName falls back by source", () => {
  assert.equal(displayName(seg(1, "them", null, 0, "x"), "Maximilian"), "Speaker");
  assert.equal(displayName(seg(1, "me", null, 0, "x"), "Maximilian"), "Maximilian");
  assert.equal(displayName(seg(1, "them", "Sarah", 0, "x"), "Maximilian"), "Sarah");
});
test("captureNote copy per mode", () => {
  assert.equal(captureNote("teams_only"), "Teams only");
  assert.equal(captureNote("all_sound"), "Capturing all computer sound");
  assert.equal(captureNote("mic_only"), "Teams isn't open — only your mic is being recorded");
});
test("footerLabel", () => {
  const note = { model: "claude-opus-5-5", inputTokens: 1200, outputTokens: 800, cacheReadTokens: 0, cacheWriteTokens: 16200, costUsd: 0.1134, createdAtUtc: "2026-11-24T14:32:00.000Z" };
  const f = footerLabel(note, (iso) => new Date(iso).toISOString().slice(11, 16));
  assert.equal(f, "Claude Opus 5.5 · 18.2k tokens · ~$0.11 · 14:32");
  assert.ok(footerLabel({ ...note, model: "claude-sonnet-5-5" }, () => "x").startsWith("Claude Sonnet 5.5"));
});

test("developer setup scripts pin the same runtime as the in-app setup", () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const manifest = JSON.parse(readFileSync(join(root, "sidecar", "runtime-manifest.json"), "utf8"));
  const ps1 = readFileSync(join(root, "scripts", "setup-runtime.ps1"), "utf8");
  const sh = readFileSync(join(root, "scripts", "setup-runtime.sh"), "utf8");
  assert.ok(ps1.includes(`$PythonVersion = '${manifest.python}'`), "setup-runtime.ps1 Python version");
  assert.ok(sh.includes(`PYTHON_VERSION="${manifest.python}"`), "setup-runtime.sh Python version");
  for (const [script, text, platform] of [["setup-runtime.ps1", ps1, "windows-x86_64"], ["setup-runtime.sh", sh, "macos-aarch64"]]) {
    for (const name of manifest.platforms[platform].models) {
      const { repo, revision } = manifest.models[name];
      assert.ok(text.includes(repo), `${script} pins ${name} to ${repo}`);
      assert.ok(text.includes(revision), `${script} pins ${name} at ${revision}`);
    }
  }
});

// ---------- enhanced notes export ----------
const NOTE_META = { title: "Rollout <sync> & review", startedAtUtc: "2026-11-25T14:00:00Z", lengthLabel: "Medium" };
const NOTE_MD = [
  "> **In short:** Rollout moves to **Thursday**.",
  "",
  "## Summary",
  "The team agreed, see [pricing](https://example.com/p?a=1&b=2) and [bad](javascript:alert(1)).",
  "",
  "## Action items",
  "| Owner | Task | Due |",
  "| --- | --- | --- |",
  "| **Tom** | Chase *QA* | Today |",
  "| Max | Pricing sheet | |",
  "",
  "- [ ] Book room",
  "- [x] Send deck",
  "",
  "1. One",
  "2. Two",
  "   - nested `a<b`",
  "",
  '<img src="https://example.com/t.png"><script>alert(2)</script>',
  "",
  "Emoji 🎉 and em dash —",
].join("\n");

test("this month's spend: notes, plus clean-ups once there are any", () => {
  assert.equal(spendLine({ usd: 2.4, notes: 14, cleanupUsd: 0, cleanups: 0 }), "~$2.40 across 14 notes (estimated from list prices).");
  assert.equal(
    spendLine({ usd: 0, notes: 1, cleanupUsd: 0.0312, cleanups: 140 }),
    "~$0.00 across 1 note · ~$0.03 across 140 clean-ups (estimated from list prices).",
  );
  // Clean-ups only: no "~$0.00 across 0 notes" in front.
  assert.equal(spendLine({ usd: 0, notes: 0, cleanupUsd: 0.0001, cleanups: 1 }), "<$0.01 across 1 clean-up (estimated from list prices).");
  assert.equal(spendLine({ usd: 0, notes: 0, cleanupUsd: 0, cleanups: 0 }), "~$0.00 across 0 notes (estimated from list prices).");
});
test("notes export: plain text has no Markdown symbols and keeps structure", () => {
  const t = plainText(NOTE_MD, NOTE_META);
  assert.ok(t.startsWith("Rollout <sync> & review\n"), t.slice(0, 40));
  for (const bad of ["**", "##", "| ", "](", "- [", "<img", "<script"]) assert.ok(!t.includes(bad), `found ${bad}\n${t}`);
  assert.ok(t.includes("In short: Rollout moves to Thursday."));
  assert.ok(t.includes("pricing (https://example.com/p?a=1&b=2)"));
  assert.ok(t.includes("• Tom (Task: Chase QA; Due: Today)"), t);
  assert.ok(t.includes("• Max (Task: Pricing sheet)"), t);
  assert.ok(t.includes("☐ Book room") && t.includes("☑ Send deck"));
  assert.ok(t.includes("1. One") && t.includes("2. Two") && t.includes("   • nested a<b"), t);
  assert.ok(t.includes("Emoji 🎉 and em dash —"));
  assert.ok(!t.includes("javascript:"));
});

test("notes export: email HTML is inline-styled, escaped and drops unsafe content", () => {
  const h = emailHtml(NOTE_MD, NOTE_META);
  assert.ok(h.startsWith('<div style="font-family:'));
  assert.ok(h.includes(">Rollout &lt;sync&gt; &amp; review</h1>"));
  assert.ok(/<table [^>]*style="border-collapse: collapse;/.test(h));
  assert.ok(/<th style="border: 1px solid #c9d1d9;[^"]*">Owner<\/th>/.test(h), "header cells carry their own borders (Outlook)");
  assert.ok(h.includes("<strong>Tom</strong>") && h.includes("<em>QA</em>"));
  assert.ok(h.includes('href="https://example.com/p?a=1&amp;b=2"'));
  assert.ok(!h.includes("javascript:") && !h.includes("<img") && !h.includes("<script") && !h.includes("<input"), h);
  assert.ok(h.includes("☐ Book room") && h.includes("☑ Send deck"));
  assert.ok(h.includes("<code") && h.includes("a&lt;b"));
  assert.ok(!h.includes('<p style="margin: 0 0 10px;"></p>'), "a paragraph of only raw HTML leaves no empty <p>");
});

test("notes export: review fixes — multi-paragraph items, inline code entities", () => {
  const h = emailHtml("- [ ] First para\n\n  Second para\n- plain item\n\nUse `&amp;` and `a<b`", NOTE_META);
  const items = h.match(/<li [^>]*>[\s\S]*?<\/li>/g);
  assert.equal(items.length, 2, h);
  const opens = (s, re) => (s.match(re) || []).length;
  for (const li of items) assert.equal(opens(li, /<p\b/g), opens(li, /<\/p>/g), `balanced <p> in ${li}`);
  assert.ok(/<li [^>]*><p [^>]*>☐ First para<\/p><p [^>]*>Second para<\/p><\/li>/.test(items[0]), items[0]);
  assert.ok(h.includes("&amp;amp;") && h.includes("a&lt;b"), "inline code keeps a literal &amp;");
  assert.ok(plainText("`&amp;`", NOTE_META).includes("&amp;"));
  assert.ok(!/<(style|link|meta)\b/.test(h), "a fragment: no <style>, mail apps drop it");
});

test("notes export: PDF page is complete and keeps rows/items whole", () => {
  const doc = pdfDocument(NOTE_MD, NOTE_META);
  assert.ok(doc.startsWith("<!doctype html>") && doc.includes('<meta charset="utf-8">'));
  assert.ok(doc.includes("@page") && doc.includes("break-inside: avoid"));
  assert.ok(doc.includes(emailHtml(NOTE_MD, NOTE_META)));
});

test("notes export: file name", () => {
  assert.equal(exportFileName(NOTE_META), "Rollout <sync> & review – 25 Nov 2026 (Medium notes)");
  assert.equal(exportFileName({ ...NOTE_META, title: "  " }), "Meeting notes – 25 Nov 2026 (Medium notes)");
});

await testAsync("notes export: Word file is a real .docx with headings, table and list", async () => {
  const bytes = await buildDocx(NOTE_MD, NOTE_META);
  assert.deepEqual([...bytes.slice(0, 4)], [0x50, 0x4b, 0x03, 0x04], "zip signature");
  const zip = await JSZip.loadAsync(bytes);
  const xml = await zip.file("word/document.xml").async("string");
  const text = xml.replace(/<[^>]+>/g, "");
  for (const want of ["Rollout &lt;sync&gt; &amp; review", "Summary", "Action items", "Owner", "Chase", "☐ ", "Book room", "☑ ", "nested", "🎉"])
    assert.ok(text.includes(want), `document.xml lacks ${want}`);
  assert.ok(xml.includes('w:val="Title"') && xml.includes('w:val="Heading2"'), "title + heading styles");
  assert.ok(xml.includes("<w:tbl>") && xml.includes("<w:tblHeader"), "a real table with a header row");
  assert.ok(xml.includes("<w:numPr>"), "real list numbering/bullets");
  assert.ok(!text.includes("**") && !text.includes("javascript") && !text.includes("<script"));
  const rels = await zip.file("word/_rels/document.xml.rels").async("string");
  assert.ok(rels.includes("https://example.com/p?a=1&amp;b=2"), "hyperlink kept");
  assert.ok(!rels.includes("javascript:"));
});

await testAsync("notes export: each numbered list in Word restarts at 1", async () => {
  const zip = await JSZip.loadAsync(await buildDocx("1. a\n2. b\n\nBetween.\n\n1. c\n2. d", NOTE_META));
  const xml = await zip.file("word/document.xml").async("string");
  const ids = [...xml.matchAll(/<w:numId w:val="(\d+)"\/>/g)].map((m) => m[1]);
  assert.equal(ids.length, 4, xml);
  assert.equal(ids[0], ids[1]);
  assert.equal(ids[2], ids[3]);
  assert.notEqual(ids[0], ids[2], "second list uses its own numbering instance");
});

console.log(failed ? `${failed} test(s) failed` : "all unit tests passed");
process.exit(failed ? 1 : 0);
