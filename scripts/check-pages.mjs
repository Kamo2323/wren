// Behaviour checks for the Task 7 pages in mock mode (no Tauri): keyboard, saving, undo, banner.
// Run after `npm run build`:  node scripts/check-pages.mjs   (exit code 1 on any failure)
import { createServer } from "node:http";
import { existsSync, readFileSync, statSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { launchEdge } from "./lib/browser.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(root, "dist");
const MIME = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml" };
const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://localhost");
  let file = join(dist, decodeURIComponent(url.pathname));
  if (existsSync(file) && statSync(file).isDirectory()) file = join(file, "index.html");
  if (!file.startsWith(dist) || !existsSync(file)) return void res.writeHead(404).end();
  res.writeHead(200, { "content-type": MIME[extname(file)] ?? "application/octet-stream" }).end(readFileSync(file));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;
const browser = await launchEdge();
const ctx = await browser.newContext({ viewport: { width: 1200, height: 800 } });
const page = await ctx.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(e.message));
page.on("console", (m) => m.type() === "error" && errors.push(`console.error: ${m.text()} ${m.location()?.url ?? ""}`));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
let failed = 0;
const check = (name, ok, detail = "") => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
  if (!ok) failed++;
};
const toast = () => page.locator(".toast").textContent().catch(() => "");

// History: Ctrl+F focuses search, search narrows, Escape restores
await page.goto(`${base}/index.html#history`);
await page.waitForSelector(".tcard");
const all = await page.locator(".tcard").count();
await page.keyboard.press("Control+f");
check("Ctrl+F focuses the search box", await page.evaluate(() => document.activeElement?.classList.contains("search__input")));
await page.keyboard.type("settings");
await wait(400);
const some = await page.locator(".tcard").count();
check("search narrows the list", some > 0 && some < all, `${all} → ${some}`);
check("matches are marked", (await page.locator(".tcard mark").count()) >= some);
await page.keyboard.press("Escape");
await wait(300);
check("Escape clears the search", (await page.locator(".tcard").count()) === all);

// Fix a word: the card keeps the fix after leaving the page (the store rewrote it), and Undo works
await page.goto(`${base}/index.html#history`);
await page.waitForSelector(".tcard");
const card = page.locator(".tcard", { hasText: "normaliseEngineStatus" });
await card.hover();
await card.getByRole("button", { name: "Fix a word" }).click();
await page.locator(".tcard.is-fixing .word", { hasText: "normaliseEngineStatus" }).click();
await page.keyboard.type("normaliseEngineState");
await page.keyboard.press("Enter");
await wait(300);
check("fix shows Undo in its toast", (await toast())?.startsWith("Fixed") && (await page.locator(".toast__action").count()) === 1);
await page.evaluate(() => (location.hash = "#home"));
await page.waitForSelector(".page--home");
await page.evaluate(() => (location.hash = "#history"));
await page.waitForSelector(".tcard");
check("the fix is still there after leaving History", (await page.locator(".tcard", { hasText: "normaliseEngineState " }).count()) === 1);
const fixCard = page.locator(".tcard", { hasText: "normaliseEngineState " });
await fixCard.hover();
await fixCard.getByRole("button", { name: "Fix a word" }).click();
await page.locator(".tcard.is-fixing .word", { hasText: "Rename" }).click();
await page.keyboard.type("Retitle");
await page.keyboard.press("Enter");
await wait(250);
await page.locator(".toast__action").click();
await wait(300);
check("Undo on a fix puts the card text back", (await page.locator(".tcard", { hasText: "Rename this function" }).count()) === 1, await toast());

// Dictionary: arrow keys move tabs; add word; undo a removal
await page.goto(`${base}/index.html#dictionary`);
await page.waitForSelector(".wordchip");
await page.getByRole("tab", { name: /Words/ }).focus();
await page.keyboard.press("ArrowRight");
check("ArrowRight moves to Replacements", (await page.getByRole("tab", { selected: true }).textContent())?.startsWith("Replacements"));
await page.keyboard.press("ArrowLeft");
const before = await page.locator(".wordchip").count();
await page.locator(".addbar input").fill("Nginx");
await page.keyboard.press("Enter");
await wait(250);
check("Add word adds a chip", (await page.locator(".wordchip").count()) === before + 1);
check("duplicate is refused", await (async () => {
  await page.locator(".addbar input").fill("NGINX");
  return page.getByRole("button", { name: "Add word" }).isDisabled();
})());
await page.locator(".addbar input").fill("");
await page.getByRole("button", { name: "Remove Tauri" }).click();
await wait(250);
check("remove shows Undo toast", (await toast())?.includes("Removed") && (await page.locator(".toast__action").count()) === 1);
await page.locator(".toast__action").click();
await wait(300);
check("Undo puts the word back", (await page.locator(".wordchip", { hasText: "Tauri" }).count()) === 1);
await page.getByRole("tab", { name: /Replacements/ }).click();
await page.locator(".drow input, .drow .toggle").first().click();
await wait(150);
check("replacement can be switched off", (await page.locator(".drow.is-muted").count()) === 1);
await page.getByRole("tab", { name: /Learned/ }).click();
await wait(200);
const firstLearned = page.locator(".drow", { hasText: "cash" });
await firstLearned.getByRole("button", { name: "Undo" }).click();
await wait(250);
await page.locator(".toast__action").click();
await wait(300);
check("undoing an Undo restores a candidate as a candidate", (await firstLearned.textContent())?.includes("Seen once"));
await firstLearned.getByRole("button", { name: "Keep" }).click();
await wait(250);
{ const t = await toast(); check("Keep promotes a candidate", t?.startsWith("Kept"), t); }
await page.locator(".drow").nth(1).getByRole("button", { name: /Delete/ }).click();
await page.locator(".drow").nth(1).getByRole("button", { name: "Delete?" }).click();
await wait(250);
check("Delete asks, then deletes", (await toast())?.startsWith("Deleted"));

// Settings: typing speed saves and Stats uses it; invalid value is put back
await page.goto(`${base}/index.html#settings`);
await page.waitForSelector("#typing-wpm");
await page.locator("#typing-wpm").fill("70");
await page.keyboard.press("Enter");
await wait(200);
await page.locator("#typing-wpm").fill("7");
check("no error while still typing", (await page.locator("#typing-wpm").getAttribute("aria-invalid")) !== "true");
await page.locator("#typing-wpm").fill("70");
await page.keyboard.press("Enter");
check("Reset link appears after a change", (await page.getByRole("button", { name: /Reset to 52/ }).count()) === 1);
await page.getByRole("link", { name: "Stats" }).count(); // no-op
await page.locator(".nav-item", { hasText: "Stats" }).click();
await page.waitForSelector(".stats-foot");
check("Stats uses the new typing speed", (await page.locator(".stats-foot").textContent())?.includes("70 wpm"));
await page.locator(".nav-item", { hasText: "Settings" }).click();
await page.waitForSelector("#typing-wpm");
check("typing speed kept after leaving the page", (await page.locator("#typing-wpm").inputValue()) === "70");
await page.locator("#typing-wpm").fill("5");
await page.keyboard.press("Tab");
check("out-of-range speed is put back", (await page.locator("#typing-wpm").inputValue()) === "70");
check("out-of-range speed is flagged", (await page.locator("#typing-wpm").getAttribute("aria-invalid")) === "true" && (await page.locator(".wpm__error").count()) === 1);
await page.getByRole("button", { name: "Test microphone" }).click();
await wait(300);
check("mic test names the device it reads", (await page.locator(".mictest__note").textContent())?.includes("Listening to"));
await page.getByRole("button", { name: "Stop test" }).click();
await page.getByRole("switch", { name: "Sounds" }).click();
check("toggle flips", (await page.getByRole("switch", { name: "Sounds" }).getAttribute("aria-checked")) === "true");
await page.getByRole("button", { name: /Delete all/ }).click();
await page.getByRole("button", { name: "Delete all", exact: true }).click();
await wait(300);
check("Delete all reports how many", /Deleted \d+ dictations/.test((await toast()) ?? ""), await toast());
await page.locator(".nav-item", { hasText: "History" }).click();
await page.waitForSelector(".empty");
check("History is empty afterwards", (await page.locator(".tcard").count()) === 0);

// Banner: shown with other apps, Details goes to Settings, dismiss hides it
await page.goto(`${base}/index.html?others=Glaido#home`);
await page.waitForSelector(".banner");
check("banner names Glaido", (await page.locator(".banner").textContent())?.includes("Glaido is running"));
await page.getByRole("button", { name: "Details" }).click();
await page.waitForSelector(".setcard");
check("Details opens Settings", (await page.locator(".page__title").textContent()) === "Settings");
await page.evaluate(() => document.querySelector(".banner")?.setAttribute("data-probe", "1"));
await page.locator(".nav-item", { hasText: "Stats" }).click();
await page.waitForSelector(".stats-foot");
check("banner is not remounted on a page switch", (await page.locator(".banner[data-probe]").count()) === 1);
await page.getByRole("button", { name: "Hide this warning" }).click();
check("banner can be hidden", (await page.locator(".banner").count()) === 0);
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".tcard");
check("no banner when nothing else runs", (await page.locator(".banner").count()) === 0);

// UX polish (Task 11): plain engine words; when something is wrong the footer opens Settings at the engine
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".tcard");
check("engine footer says Ready (no GPU jargon)", (await page.locator(".engine__text").textContent()) === "Ready");
check("a healthy engine footer is not a button", (await page.locator("button.engine").count()) === 0);
await page.goto(`${base}/index.html?engine=failed#home`);
await page.waitForSelector("button.engine");
check("failed engine footer says Speech stopped", (await page.locator(".engine__text").textContent()) === "Speech stopped");
await page.locator("button.engine").click();
await page.waitForSelector("#set-engine");
await wait(900);
check(
  "the footer opens Settings at the speech engine",
  await page.evaluate(() => {
    // at the top of the panel, or as near as the page can scroll (the group is near the end)
    const panel = document.querySelector(".panel");
    const t = document.getElementById("set-engine").getBoundingClientRect().top - panel.getBoundingClientRect().top;
    const atEnd = panel.scrollTop >= panel.scrollHeight - panel.clientHeight - 1;
    return t >= 0 && (t < 120 || (atEnd && t < panel.clientHeight / 2));
  }),
);
// Pages switch at once after the first visit: a revisit paints complete, with no entrance replay
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".tcard");
await wait(1600);
await page.locator(".nav-item", { hasText: "Stats" }).click();
await page.waitForSelector(".stats-foot");
await page.locator(".nav-item", { hasText: "Home" }).click();
check("Home revisit paints its hero metrics at once (cached)", (await page.locator(".stats--loading").count()) === 0 && (await page.locator(".stat").count()) === 3);
check(
  "no entrance replay on revisit",
  await page.evaluate(() => [...document.querySelectorAll(".stat, .hero, .tcard")].every((el) => getComputedStyle(el).animationName === "none")),
);

// Redesign checks: Light & Dark theme switching, real app icons, Home -> History link, History day filter, Stats 7d/30d toggle
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".tcard");
check("Home hero waveform canvas renders", (await page.locator(".hero__canvas").count()) === 1);
check(
  "transcript cards render real app icons",
  await page.evaluate(() => {
    const imgs = [...document.querySelectorAll(".tcard .app-icon img")];
    return imgs.length > 0 && imgs.every((img) => img.complete && img.naturalWidth > 0);
  }),
);
await page.locator(".toolbar .theme-switch button", { hasText: "Dark" }).click();
check("Dark theme switch applies data-theme=dark", (await page.evaluate(() => document.documentElement.dataset.theme)) === "dark");
await page.locator(".toolbar .theme-switch button", { hasText: "Light" }).click();
check("Light theme switch applies data-theme=light", (await page.evaluate(() => document.documentElement.dataset.theme)) === "light");
await page.locator(".nav-item", { hasText: "Settings" }).click();
await page.waitForSelector("#set-appearance");
await page.locator("#set-appearance").locator("..").getByRole("button", { name: "Dark" }).click();
check("Settings Appearance updates theme to dark", (await page.evaluate(() => document.documentElement.dataset.theme)) === "dark");
await page.locator(".nav-item", { hasText: "Home" }).click();
await page.waitForSelector(".hero");
await page.getByRole("button", { name: /View history/ }).click();
await page.waitForSelector(".history-day-select");
await page.waitForSelector(".tcard");
check("Home View history link opens History", (await page.locator(".page__title").textContent()) === "History");
const allHist = await page.locator(".tcard").count();
await page.locator(".history-day-select").selectOption("Today");
await wait(100);
const todayHist = await page.locator(".tcard").count();
check("History day filter narrows to Today", todayHist > 0 && todayHist < allHist, `${allHist} → ${todayHist}`);
await page.locator(".history-day-select").selectOption("all");
await page.locator(".nav-item", { hasText: "Stats" }).click();
await page.waitForSelector(".chart--daily");
check("Stats Words per day defaults to 30 bars", (await page.locator(".chart--daily .chart__col").count()) === 30);
await page.getByRole("button", { name: "7 days" }).click();
check("Stats 7 days toggle narrows Words per day to 7 bars", (await page.locator(".chart--daily .chart__col").count()) === 7);
await page.getByRole("button", { name: "30 days" }).click();
check("Stats 30 days toggle restores 30 bars", (await page.locator(".chart--daily .chart__col").count()) === 30);

check("no page errors or console errors", errors.length === 0, errors.join("; "));

// App updater: hidden when idle, subtle sidebar button when available, one-click starts update, Settings check works
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".tcard");
check("sidebar update button stays hidden when up to date", (await page.locator(".update-pill").count()) === 0);
await page.goto(`${base}/index.html?update=available#home`);
await page.waitForSelector(".update-pill--available");
check("sidebar shows non-intrusive update button with version", (await page.locator(".update-pill").textContent())?.includes("Update · v0.1.2"));
await page.locator(".update-pill--available").click();
await page.waitForFunction(() => document.querySelector(".update-pill")?.textContent?.includes("Restarting"));
check("clicking sidebar update button downloads and transitions to Restarting", (await page.locator(".update-pill").textContent())?.includes("Restarting"));
await page.goto(`${base}/index.html#settings`);
await page.waitForSelector("#set-updates");
await page.getByRole("button", { name: "Check for updates" }).click();
await page.waitForFunction(() => document.body.textContent?.includes("You're on the latest version."));
check("Settings Check for updates reports Up to date", (await page.locator("section", { hasText: "Updates" }).textContent())?.includes("Up to date"));
await page.goto(`${base}/index.html?update=retry#home`);
await page.waitForSelector(".update-pill--error");
check("failed download offers Retry update pill in sidebar", (await page.locator(".update-pill--error").textContent())?.includes("Retry update"));

// Load failures: a short "Couldn't load · Try again", never a misleading empty page
for (const [hash, what] of [
  ["history", "your history"],
  ["dictionary", "your dictionary"],
  ["stats", "your stats"],
  ["settings", "your settings"],
]) {
  await page.goto(`${base}/index.html?fail=1#${hash}`);
  await page.waitForSelector(".empty--error");
  const text = await page.locator(".empty--error").textContent();
  check(`${hash}: load error state`, text.includes(`Couldn't load ${what}`) && text.includes("Try again"), text.slice(0, 40));
}

// Platform adaptation: macOS uses fn + Control, ⌘F, Start at login, Accessibility banner when untrusted; Windows keeps Ctrl + Win.
await page.goto(`${base}/index.html?platform=macos&accessibility=0#settings`);
await page.waitForSelector("#set-dictation");
check("macOS Accessibility banner appears when permission is missing", (await page.locator(".banner").textContent())?.includes("Accessibility permission needed"));
check("macOS Settings uses Start at login and macOS default", await page.evaluate(() => {
  const t = document.body.textContent ?? "";
  return t.includes("Start at login") && t.includes("macOS default") && t.includes("this Mac");
}));
await page.goto(`${base}/index.html?platform=macos#history`);
await page.waitForSelector(".search__kbd");
check("macOS History shows ⌘F shortcut badge", (await page.locator(".search__kbd").textContent())?.trim() === "⌘F");
await page.goto(`${base}/popup.html?state=handsfree&platform=macos&hold=1`);
await page.waitForSelector(".hint");
check("macOS popup hands-free hint says fn + Control", await page.evaluate(() => {
  const t = document.querySelector(".hint")?.textContent ?? "";
  return t.includes("fn") && t.includes("Control");
}));
await page.goto(`${base}/index.html?platform=windows#settings`);
await page.waitForSelector("#set-dictation");
check("Windows Settings preserves Start with Windows and Windows default", await page.evaluate(() => {
  const t = document.body.textContent ?? "";
  return t.includes("Start with Windows") && t.includes("Windows default") && t.includes("this PC");
}));
await page.goto(`${base}/popup.html?state=handsfree&platform=windows&hold=1`);
await page.waitForSelector(".hint");
check("Windows popup hands-free hint preserves Ctrl + Win", await page.evaluate(() => {
  const t = document.querySelector(".hint")?.textContent ?? "";
  return t.includes("Ctrl") && t.includes("Win");
}));

// ---------------- Meetings (plan Task 11) ----------------
const selectedTab = () => page.locator('[role="tab"][aria-selected="true"]').first().textContent().then((t) => t?.trim());
await page.goto(`${base}/index.html#meetings`);
await page.waitForSelector(".meeting-card");
check("Meetings list shows day groups of cards", (await page.locator(".meeting-card").count()) === 3 &&
  (await page.locator(".day__label").allTextContents()).some((t) => /Today|Yesterday/.test(t)));
check("Start button when nothing records", (await page.locator(".meetings-start").textContent())?.includes("Start meeting notes"));
check("Sidebar has Meetings below Home", await page.evaluate(() => {
  const ids = [...document.querySelectorAll(".nav-item")].map((b) => b.getAttribute("data-page"));
  return ids.indexOf("meetings") === ids.indexOf("home") + 1;
}));

// Ended meeting opens on Enhanced, with the footer.
await page.locator(".meeting-card", { hasText: "Weekly sync with Sarah" }).click();
await page.waitForSelector(".meeting-view");
check("ended meeting opens on the Enhanced tab", (await selectedTab())?.startsWith("Enhanced"));
await page.waitForSelector(".md h2");
check("enhanced notes render markdown headings", (await page.locator(".md h2").first().textContent()) === "Summary");
check("notes open with an 'In short' headline box", (await page.locator(".md > blockquote:first-child").textContent())?.includes("In short"));
check("raw HTML in notes is never rendered (no remote loads)", (await page.locator(".md img, .md video, .md [src]").count()) === 0);
check("action items render as a table", (await page.locator(".md table th").allTextContents()).join("|").includes("Owner"));
const urlBefore = page.url();
await page.locator(".md a").first().click();
await wait(200);
check("links in notes don't navigate Wren's window", page.url() === urlBefore);
check("…and go to the browser instead", (await page.evaluate(() => window.__wrenOpenedLink)) === "https://example.com/pricing");
check("enhanced footer shows model, tokens and cost", /Claude Opus 5\.5 · [\d.]+k tokens · ~\$\d+\.\d\d/.test((await page.locator(".enh-footer").textContent()) ?? ""));
await page.getByRole("button", { name: "Short", exact: true }).click();
await page.waitForSelector(".enh-writing", { timeout: 2000 }).catch(() => {});
await page.waitForFunction(() => !document.querySelector(".enh-writing") && document.querySelector(".enh-footer"), null, { timeout: 5000 });
check("Short streams in and gets its footer", (await page.locator(".md h2").first().textContent()) === "Summary");

// Transcript: muted tag, rename all.
await page.getByRole("tab", { name: "Transcript" }).click();
await page.waitForSelector(".tline");
check("muted lines are tagged", (await page.locator(".tline.is-muted .tline__tag").first().textContent())?.trim() === "muted");
const sarahBefore = await page.locator(".trun__name", { hasText: "Sarah Jones" }).count();
await page.locator(".trun__name", { hasText: "Sarah Jones" }).first().click();
await page.getByRole("button", { name: /Rename all/ }).click();
await page.locator(".rename__input").fill("Sarah J.");
await page.keyboard.press("Enter");
await wait(250);
check("Rename all updates every Sarah Jones run", sarahBefore > 1 &&
  (await page.locator(".trun__name", { hasText: "Sarah J." }).count()) === sarahBefore &&
  (await page.locator(".trun__name", { hasText: "Sarah Jones" }).count()) === 0);
check("an unnamed line shows as Speaker", (await page.locator(".trun__name", { hasText: "Speaker" }).count()) >= 1);

// My notes survive switching tabs (they used to reload the old text and overwrite it).
await page.getByRole("tab", { name: "My notes" }).click();
await page.locator(".notes-editor").click();
await page.keyboard.type("Keep this line");
await wait(900);
await page.getByRole("tab", { name: "Transcript" }).click();
await page.getByRole("tab", { name: "My notes" }).click();
await wait(150);
check("My notes keep what was typed after switching tabs", (await page.locator(".notes-editor").textContent())?.includes("Keep this line"));
await page.locator(".notes-editor").click();
await page.keyboard.press("End");
await page.keyboard.type(" and more");
await wait(900);
await page.getByRole("tab", { name: "Transcript" }).click();
await page.getByRole("tab", { name: "My notes" }).click();
check("…and further edits add to them", (await page.locator(".notes-editor").textContent())?.includes("Keep this line and more"));

// No key: the Enhanced tab sends you to Settings.
await page.goto(`${base}/index.html?claude=nokey#meetings`);
await page.locator(".meeting-card", { hasText: "Supplier call" }).click();
await page.getByRole("tab", { name: "Enhanced" }).click();
await page.waitForSelector(".enh-empty");
check("no key: Enhanced shows an Open Settings action", (await page.locator(".enh-empty").textContent())?.includes("Add your Claude API key") &&
  (await page.getByRole("button", { name: "Open Settings" }).count()) === 1);

// Network error mid-stream: error with Retry, no partial note kept.
await page.goto(`${base}/index.html?claude=error#meetings`);
await page.locator(".meeting-card", { hasText: "Supplier call" }).click();
await page.getByRole("tab", { name: "Enhanced" }).click();
await page.getByRole("button", { name: /Write notes/ }).click();
await page.waitForSelector(".enh-error", { timeout: 3000 });
check("stream error shows Retry and keeps no partial text", (await page.getByRole("button", { name: "Retry" }).count()) === 1 &&
  (await page.locator(".md").count()) === 0);

// Live meeting.
await page.goto(`${base}/index.html?meeting=recording#meetings`);
await page.waitForSelector(".meetings-start");
check("Start becomes Open current meeting while one records", (await page.locator(".meetings-start").textContent())?.includes("Open current meeting"));
await page.waitForSelector('.nav-item[data-page="meetings"] .nav-item__live', { timeout: 2000 }).catch(() => {});
check("sidebar shows the recording dot", (await page.locator('.nav-item[data-page="meetings"] .nav-item__live').count()) === 1);
await page.locator(".meetings-start").click();
await page.waitForSelector(".meeting-view");
check("recording meeting opens on Transcript", (await selectedTab())?.startsWith("Transcript"));
check("pending row shows while someone talks", (await page.locator(".tline--pending").count()) >= 1);
const t1 = await page.locator(".mh-timer").textContent();
await wait(1300);
check("the recording timer ticks", (await page.locator(".mh-timer").textContent()) !== t1);
await page.getByRole("button", { name: "Pause" }).click();
await wait(200);
check("Pause shows Paused and Resume", (await page.locator(".mh-status").textContent())?.includes("Paused") && (await page.getByRole("button", { name: "Resume" }).count()) === 1);
await page.getByRole("button", { name: "Resume" }).click();
await page.getByRole("button", { name: "Stop" }).click();
await page.waitForFunction(() => document.querySelector('[role="tab"][aria-selected="true"]')?.textContent?.startsWith("Enhanced"), null, { timeout: 3000 });
check("Stop switches to Enhanced", true);
check("'Writing…' shows straight after Stop (no Write notes button meanwhile)",
  (await page.locator(".enh-writing").count()) === 1 && (await page.getByRole("button", { name: /Write notes/ }).count()) === 0);

// Big transcript: jump to live and smooth scrolling.
await page.goto(`${base}/index.html?meeting=recording&big=1#meetings`);
await page.locator(".meetings-start").click();
await page.waitForSelector(".tline");
await page.evaluate(() => { const el = document.querySelector(".transcript"); if (el) el.scrollTop = 0; });
await wait(250);
check("Jump to live appears when scrolled up", await page.locator(".jump-live").isVisible());
const worst = await page.evaluate(async () => {
  const el = document.querySelector(".transcript");
  let last = performance.now(), worst = 0;
  for (let i = 0; i < 40; i++) {
    el.scrollTop += 600;
    await new Promise((r) => requestAnimationFrame(r));
    const now = performance.now();
    worst = Math.max(worst, now - last);
    last = now;
  }
  return worst;
});
check("2,500-line transcript scrolls without long frames", worst < 60, `${worst.toFixed(1)} ms worst frame`);

// Settings: Meeting notes section; the key is never shown.
await page.goto(`${base}/index.html?claude=nokey#settings`);
await page.waitForSelector("#set-meetings");
check("Settings has a Meeting notes section", (await page.locator("#set-meetings").textContent())?.includes("Meeting notes"));
const keyInput = page.locator("#claude-key");
check("API key field is a password field", (await keyInput.getAttribute("type")) === "password");
await keyInput.fill("sk-ant-api03-" + "x".repeat(40));
await page.getByRole("button", { name: "Save key" }).click();
await page.waitForSelector(".key-saved", { timeout: 3000 });
check("saved key: status shown, key not rendered anywhere", !(await page.content()).includes("x".repeat(40)) &&
  (await page.locator(".key-saved").textContent())?.includes("Key saved"));
check("model picker offers Opus 5.5 and Sonnet 5.5", (await page.locator("#claude-model option").allTextContents()).join("|").includes("Opus 5.5") &&
  (await page.locator("#claude-model option").allTextContents()).join("|").includes("Sonnet 5.5"));
check("this month's spend is shown", /~\$\d+\.\d\d across \d+ notes/.test((await page.locator("section:has(#set-meetings)").textContent()) ?? ""));

// Quit while recording asks first.
await page.goto(`${base}/index.html?meeting=recording#meetings`);
await page.waitForSelector(".meetings-start");
await page.evaluate(() => window.dispatchEvent(new CustomEvent("wren-mock-confirm-quit")));
await page.waitForSelector(".quit-confirm", { timeout: 2000 }).catch(() => {});
check("quit while recording asks to stop first", (await page.locator(".quit-confirm").textContent())?.includes("Stop meeting notes and quit?"));

for (const [failure, action] of [["fail-once", "Try again"], ["fail", "Quit anyway"]]) {
  await page.goto(`${base}/index.html?meeting=recording&notesSave=${failure}#meetings`);
  await page.locator(".meetings-start").click();
  await page.getByRole("tab", { name: "My notes" }).click();
  await page.locator(".notes-editor").fill("Keep this last edit");
  if (failure === "fail") {
    await page.getByText("Couldn't save", { exact: true }).waitFor();
    await page.getByRole("tab", { name: /Transcript/ }).click();
    await page.getByRole("tab", { name: "My notes" }).click();
    check("a failed draft survives switching tabs", (await page.locator(".notes-editor").textContent())?.includes("Keep this last edit"));
  }
  await page.evaluate(() => window.dispatchEvent(new CustomEvent("wren-mock-confirm-quit")));
  await page.getByRole("button", { name: "Stop and quit" }).click();
  const next = page.getByRole("button", { name: action, exact: true });
  await next.waitFor({ timeout: 1500 }).catch(() => {});
  check(`failed final save offers ${action} and keeps the meeting recording`,
    await next.isVisible() && (await page.locator(".quit-confirm").textContent())?.includes("couldn't be saved") &&
    (await page.locator(".mh-status").textContent())?.includes("Recording"));
  if (await next.isVisible()) {
    await next.click();
    await page.getByRole("button", { name: "Stop", exact: true }).waitFor({ state: "hidden", timeout: 2000 });
    check(`${action} stops the meeting`, !(await page.getByRole("button", { name: "Stop", exact: true }).isVisible()));
    if (action === "Try again") {
      await page.locator(".back-link").click();
      await page.locator(".meeting-card", { hasText: "Design review" }).click();
      await page.getByRole("tab", { name: "My notes" }).click();
      check("Try again saves the same draft",
        (await page.locator(".notes-editor").textContent())?.includes("Keep this last edit"));
    }
  }
}

await page.goto(`${base}/index.html?meeting=recording#meetings`);
await page.locator(".meetings-start").click();
await page.getByRole("tab", { name: "My notes" }).click();
await page.evaluate(() => {
  const set = window.setTimeout.bind(window);
  window.setTimeout = (callback, delay, ...args) => set(callback, delay === 800 ? 60_000 : delay, ...args);
  window.addEventListener("wren-mock-notes-saving", () => {
    document.body.dataset.notesSavedFrom = document.querySelector('[role="tab"][aria-selected="true"]')?.textContent;
  }, { once: true });
});
await page.locator(".notes-editor").fill("Saved even with the timer throttled");
await page.evaluate(() => window.dispatchEvent(new CustomEvent("wren-mock-stop-meeting")));
await page.getByRole("tab", { name: /Enhanced/, selected: true }).waitFor();
check("tray stop starts the save before switching tabs",
  (await page.locator("body").getAttribute("data-notes-saved-from"))?.startsWith("My notes"));
await page.locator(".back-link").click();
await page.locator(".meeting-card", { hasText: "Design review" }).click();
await page.getByRole("tab", { name: "My notes" }).click();
check("tray stop flushes My notes with the debounce timer throttled",
  (await page.locator(".notes-editor").textContent())?.includes("Saved even with the timer throttled"));

// No horizontal overflow on the new screens, both themes, both sizes.
for (const theme of ["light", "dark"]) {
  for (const [w, h] of [[1200, 800], [960, 640]]) {
    await page.setViewportSize({ width: w, height: h });
    for (const url of [`?theme=${theme}#meetings`, `?theme=${theme}&meeting=recording#meetings`]) {
      await page.goto(`${base}/index.html${url}`);
      await page.waitForSelector(".meetings-start");
      if (url.includes("recording")) { await page.locator(".meetings-start").click(); await page.waitForSelector(".meeting-view"); }
      else { await page.locator(".meeting-card").first().click(); await page.waitForSelector(".meeting-view"); }
      await wait(200);
      const over = await page.evaluate(() => { const p = document.querySelector(".panel"); return p ? p.scrollWidth - p.clientWidth : 0; });
      check(`no horizontal overflow ${theme} ${w}x${h} ${url.includes("recording") ? "live" : "ended"}`, over <= 0, `${over}px`);
    }
  }
}
await page.setViewportSize({ width: 1200, height: 800 });

// Fresh profile: no owner name in the greeting; a name the user set is used.
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".page--home");
check("fresh profile greets without a name", (await page.locator(".page--home .page__title").textContent()) === "Hi there.");
await page.goto(`${base}/index.html?name=Sam#home`);
await page.waitForSelector(".page--home");
check("a saved name is used in the greeting", await page.waitForFunction(() => document.querySelector(".page--home .page__title")?.textContent === "Hi, Sam.", null, { timeout: 3000 }).then(() => true, () => false));

// First-run speech setup: opens on its own, shows sizes, runs to Ready only after "starting".
await page.goto(`${base}/index.html?setup=needed#home`);
await page.waitForSelector(".page--setup");
check("missing speech runtime opens the setup screen", (await page.locator(".page--setup .page__title").textContent()) === "Set up speech recognition");
check("setup lists what it downloads with sizes", (await page.locator(".setup-part").count()) === 3 && /GB/.test(await page.locator(".setup-space").textContent()));
check("sidebar footer asks to set up speech", (await page.locator(".engine").textContent())?.includes("Set up speech"));
await page.getByRole("button", { name: "Download and set up" }).click();
await page.waitForSelector(".setup-bar");
check("progress shows a step and a bar", (await page.locator(".setup-step__count").textContent())?.startsWith("Step"));
await page.waitForSelector(".setup-ready", { timeout: 8000 });
check("ready state appears after the engine warms up", (await page.locator(".setup-ready__title").textContent()) === "Speech is ready");
check("ready state offers the microphone and try-it checks", (await page.locator(".setup-check").count()) >= 2);
await page.getByRole("button", { name: "Start using Wren" }).click();
await page.waitForSelector(".page--home");
check("finishing setup goes Home", true);

for (const [mode, text] of [["paused", "Continue setup"], ["failed", "Try again"], ["nospace", "Try again"]]) {
  await page.goto(`${base}/index.html?setup=${mode}#home`);
  await page.waitForSelector(".setup-alert");
  check(`setup ${mode} explains itself and offers "${text}"`, (await page.getByRole("button", { name: text }).count()) === 1);
}
await page.goto(`${base}/index.html?setup=nospace&again=1#home`);
await page.waitForSelector(".setup-space");
check("not enough space is flagged", await page.locator(".setup-space.is-bad").count() === 1);
await page.goto(`${base}/index.html?setup=paused&again=1#home`);
await page.waitForSelector(".setup-alert");
await page.getByRole("button", { name: "Continue setup" }).click();
await page.waitForSelector(".setup-ready", { timeout: 8000 });
check("a paused setup resumes to Ready", true);
for (const theme of ["light", "dark"]) {
  for (const [w, h] of [[1200, 800], [960, 640]]) {
    await page.setViewportSize({ width: w, height: h });
    for (const mode of ["needed", "running", "failed", "done"]) {
      await page.goto(`${base}/index.html?theme=${theme}&setup=${mode}#setup`);
      await page.waitForSelector(".setup-card");
      const over = await page.evaluate(() => { const p = document.querySelector(".panel"); return p ? p.scrollWidth - p.clientWidth : 0; });
      check(`setup ${mode} no horizontal overflow ${theme} ${w}x${h}`, over <= 0, `${over}px`);
    }
  }
}
await page.setViewportSize({ width: 1200, height: 800 });

await browser.close();
server.close();
console.log(failed ? `${failed} check(s) failed` : "all checks passed");
process.exit(failed ? 1 : 0);
