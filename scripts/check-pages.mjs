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
const firstLearned = page.locator(".drow").first();
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
await page.getByRole("button", { name: "Hide this warning" }).click();
check("banner can be hidden", (await page.locator(".banner").count()) === 0);
await page.goto(`${base}/index.html#home`);
await page.waitForSelector(".tcard");
check("no banner when nothing else runs", (await page.locator(".banner").count()) === 0);

check("no page errors", errors.length === 0, errors.join("; "));
await browser.close();
server.close();
console.log(failed ? `${failed} check(s) failed` : "all checks passed");
process.exit(failed ? 1 : 0);
