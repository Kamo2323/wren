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

await browser.close();
server.close();
console.log(failed ? `${failed} check(s) failed` : "all checks passed");
process.exit(failed ? 1 : 0);
