// Screenshots of the UI in mock mode (no Tauri): builds with Vite, serves dist/, and captures
// Home (populated, empty, hover, copied, fix-a-word, small window) plus every popup state, into
// docs/screenshots/. Uses the installed Microsoft Edge (see scripts/lib/browser.mjs).
//
//   node scripts/screens.mjs            build + shoot everything
//   node scripts/screens.mjs --no-build shoot the existing dist/
//   node scripts/screens.mjs --only popup|home
import { spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdirSync, readFileSync, existsSync, statSync } from "node:fs";
import { dirname, extname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { launchEdge } from "./lib/browser.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const dist = join(root, "dist");
const outDir = join(root, "docs", "screenshots");
const args = process.argv.slice(2);
const only = args.includes("--only") ? args[args.indexOf("--only") + 1] : null;

if (!args.includes("--no-build")) {
  const r = spawnSync("npm", ["run", "build"], { cwd: root, stdio: "inherit", shell: true });
  if (r.status !== 0) process.exit(r.status ?? 1);
}
mkdirSync(outDir, { recursive: true });

// ---- tiny static server for dist/ ----
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript",
  ".css": "text/css",
  ".woff2": "font/woff2",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
};
const server = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://localhost");
  let file = join(dist, decodeURIComponent(url.pathname));
  if (existsSync(file) && statSync(file).isDirectory()) file = join(file, "index.html");
  if (!file.startsWith(dist) || !existsSync(file)) {
    res.writeHead(404).end("not found");
    return;
  }
  res.writeHead(200, { "content-type": MIME[extname(file)] ?? "application/octet-stream" });
  res.end(readFileSync(file));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;

const browser = await launchEdge();
const shots = [];
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

async function shoot(page, name, opts = {}) {
  const path = join(outDir, `${name}.png`);
  await page.screenshot({ path, ...opts });
  shots.push(path);
  console.log(`  ${name}.png`);
}

// ---------------- Home ----------------
if (!only || only === "home") {
  console.log("home");
  const ctx = await browser.newContext({ viewport: { width: 1200, height: 800 }, deviceScaleFactor: 1 });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.error("  page error:", e.message));

  await page.goto(`${base}/index.html#home`);
  await page.waitForSelector(".tcard");
  await wait(1100); // entry animations
  await shoot(page, "home-populated-1200x800");

  await page.setViewportSize({ width: 1100, height: 700 });
  await wait(300);
  await shoot(page, "home-populated-1100x700");

  await page.setViewportSize({ width: 960, height: 640 });
  await wait(300);
  await shoot(page, "home-min-960x640");

  await page.setViewportSize({ width: 1200, height: 800 });
  await wait(200);
  // full scroll height of the panel, to see every card
  const fullH = await page.evaluate(() => {
    const p = document.querySelector(".panel");
    return p ? p.scrollHeight + 16 : 800;
  });
  await page.setViewportSize({ width: 1200, height: Math.min(2400, fullH) });
  await wait(300);
  await shoot(page, "home-populated-full");
  await page.setViewportSize({ width: 1200, height: 800 });
  await wait(200);

  // hover a card
  const cards = page.locator(".tcard");
  await cards.nth(1).hover();
  await page.locator(".strip__col").nth(27).hover();
  await cards.nth(1).hover();
  await wait(350);
  await shoot(page, "home-card-hover");

  // hover a strip bar
  await page.locator(".strip__col").nth(27).hover();
  await wait(250);
  await shoot(page, "home-strip-hover", { clip: { x: 216, y: 0, width: 984, height: 300 } });

  // click → copied
  await ctx.grantPermissions(["clipboard-read", "clipboard-write"], { origin: base });
  await cards.nth(0).click({ position: { x: 300, y: 60 } });
  await wait(260);
  await shoot(page, "home-card-copied");
  const clip = await page.evaluate(() => navigator.clipboard.readText().catch(() => "(unreadable)"));
  console.log(`  clipboard after click: "${clip.slice(0, 60)}…"`);

  // fix a word
  await wait(1600);
  await cards.nth(1).hover();
  await cards.nth(1).getByRole("button", { name: "Fix a word" }).click();
  await wait(200);
  await shoot(page, "home-fix-pick");
  await page.locator(".tcard.is-fixing .word", { hasText: "normaliseEngineStatus" }).click();
  await page.keyboard.type("normaliseEngineState");
  await wait(200);
  await shoot(page, "home-fix-type");
  await page.keyboard.press("Enter");
  await wait(450);
  await shoot(page, "home-fix-saved");

  // delete confirm
  await cards.nth(2).hover();
  await cards.nth(2).getByRole("button", { name: "Delete" }).click();
  await wait(300);
  await shoot(page, "home-delete-confirm");

  // empty / first run
  await page.goto(`${base}/index.html?mock=empty#home`);
  await page.waitForSelector(".empty");
  await wait(1100);
  await shoot(page, "home-empty-1200x800");
  await ctx.close();
}

// ---------------- Popup ----------------
if (!only || only === "popup") {
  console.log("popup");
  const states = [
    ["listening", "state=listening", 1500],
    ["listening-no-audio-yet", "state=listening&silent=1", 900],
    ["listening-cpu", "state=listening&cpu=1", 1500],
    ["handsfree", "state=handsfree", 1500],
    ["cap-warning", "state=capWarning", 1500],
    ["cap-warning-handsfree", "state=capWarning&hf=1", 1500],
    ["warming", "state=warming", 900],
    ["transcribing", "state=transcribing", 900],
    ["transcribing-still-working", "state=transcribing&slow=1", 900],
    ["pasted", "state=pasted&hold=1", 900],
    ["nothing", "state=nothing&hold=1", 900],
    ["no-sound", "state=noSound&hold=1", 900],
    ["not-pasted", "state=notPasted&hold=1", 900],
    ["learned", "state=learned&hold=1", 900],
    ["error", "state=error&hold=1", 900],
  ];
  const backdrops = { light: "#e9e9ef", dark: "#202024" };
  const ctx = await browser.newContext({ viewport: { width: 360, height: 120 }, deviceScaleFactor: 2 });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => console.error("  page error:", e.message));
  const cells = [];
  for (const [name, q, settle] of states) {
    await page.goto(`${base}/popup.html?${q}`);
    await wait(settle);
    // transparent original (what the window really draws)
    await shoot(page, `popup-${name}`, { omitBackground: true });
    cells.push(name);
  }

  // click actions
  await page.goto(`${base}/popup.html?state=learned&hold=1`);
  await wait(600);
  await page.locator(".pill__action").click();
  await wait(450);
  await shoot(page, "popup-learned-undone", { omitBackground: true });
  cells.push("learned-undone");
  await page.goto(`${base}/popup.html?state=notPasted&hold=1`);
  await wait(600);
  await page.locator(".pill").click();
  await wait(450);
  await shoot(page, "popup-not-pasted-copied", { omitBackground: true });
  cells.push("not-pasted-copied");

  // the bars' animation must stop once the popup has hidden
  {
    const rctx = await browser.newContext({ viewport: { width: 360, height: 120 } });
    const p = await rctx.newPage();
    await p.addInitScript(() => {
      window.__raf = 0;
      const orig = window.requestAnimationFrame.bind(window);
      window.requestAnimationFrame = (cb) => {
        window.__raf++;
        return orig(cb);
      };
    });
    await p.goto(`${base}/popup.html?state=listening`);
    await wait(1000);
    const during = await p.evaluate(async () => {
      const a = window.__raf;
      await new Promise((r) => setTimeout(r, 1000));
      return window.__raf - a;
    });
    await p.evaluate(() => window.__wrenPopup.apply({ kind: "hidden" }));
    await wait(600); // exit animation + unmount
    const after = await p.evaluate(async () => {
      const a = window.__raf;
      await new Promise((r) => setTimeout(r, 1000));
      const anims = document.getAnimations().filter((x) => x.playState === "running");
      return { raf: window.__raf - a, cssAnimations: anims.length, names: anims.map((x) => `${x.animationName ?? x.transitionProperty} on ${x.effect?.target?.className}`) };
    });
    console.log(`  rAF check: ${during} calls/s while listening; after hide ${after.raf} calls/s, ${after.cssAnimations} CSS animations running ${after.names.join(", ")}`);
    if (after.raf > 2 || after.cssAnimations > 0) {
      console.error("  rAF check FAILED: animation still running after the popup hid");
      process.exitCode = 1;
    }
    await rctx.close();
  }

  // hands-free hint (first 3.5 s)
  await page.goto(`${base}/popup.html?state=handsfree`);
  await wait(700);
  await shoot(page, "popup-handsfree-hint", { omitBackground: true });

  // grow sequence: frames during the spring
  await page.goto(`${base}/popup.html?state=listening`);
  for (let i = 0; i < 8; i++) {
    await shoot(page, `popup-seq-grow-${i}`, { omitBackground: true });
    await wait(25);
  }
  // bars over time
  for (let i = 0; i < 6; i++) {
    await wait(140);
    await shoot(page, `popup-seq-bars-${i}`, { omitBackground: true });
  }
  await ctx.close();

  // reduced motion
  const rctx = await browser.newContext({ viewport: { width: 360, height: 120 }, deviceScaleFactor: 2, reducedMotion: "reduce" });
  const rpage = await rctx.newPage();
  await rpage.goto(`${base}/popup.html?state=transcribing`);
  await wait(600);
  await shoot(rpage, "popup-transcribing-reduced-motion", { omitBackground: true });
  await rctx.close();

  // contact sheet: every state on a light and a dark desktop
  const sheet = await browser.newPage({ viewport: { width: 1000, height: 400 }, deviceScaleFactor: 1 });
  const img = (n) => `data:image/png;base64,${readFileSync(join(outDir, `popup-${n}.png`)).toString("base64")}`;
  const rows = [...cells, "handsfree-hint", "transcribing-reduced-motion"]
    .map(
      (n) => `<div class="row"><div class="lbl">${n}</div>${Object.entries(backdrops)
        .map(([, c]) => `<div class="cell" style="background:${c}"><img src="${img(n)}" width="360" height="120"></div>`)
        .join("")}</div>`,
    )
    .join("");
  await sheet.setContent(
    `<html><body style="margin:0;font:13px Segoe UI;background:#fff"><style>.row{display:flex;align-items:center;gap:12px;padding:6px 12px}.lbl{width:200px;color:#444}.cell{border-radius:8px;overflow:hidden;line-height:0}</style>${rows}</body></html>`,
  );
  await wait(200);
  await shoot(sheet, "popup-contact-sheet", { fullPage: true });
  // motion sheet: the grow frames then the bars over time
  const seq = [...Array(8).keys()].map((i) => `seq-grow-${i}`).concat([...Array(6).keys()].map((i) => `seq-bars-${i}`));
  await sheet.setViewportSize({ width: 760, height: 200 });
  await sheet.setContent(
    `<html><body style="margin:0;background:#fff">${seq
      .map((n) => `<div style="display:inline-block;margin:2px;background:#e9e9ef;line-height:0"><img src="${img(n)}" width="360" height="120"></div>`)
      .join("")}</body></html>`,
  );
  await wait(200);
  await shoot(sheet, "popup-motion-sheet", { fullPage: true });
  await sheet.close();
}

await browser.close();
server.close();
console.log(`${shots.length} screenshots in ${outDir}`);
