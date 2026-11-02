// Switches the running Wren's main window to a page through WebView2's DevTools port (no keys, no
// clicks reach Windows). Used by scripts/real-screens.ps1.
//   node scripts/lib/cdp-nav.mjs <port> <page> [scroll]
//   page: home | history | dictionary | stats | settings; scroll: top | bottom | <px>
import { chromium } from "playwright-core";

const [port, page, scroll = "top"] = process.argv.slice(2);
const browser = await chromium.connectOverCDP(`http://127.0.0.1:${port}`);
try {
  const pages = browser.contexts().flatMap((c) => c.pages());
  const main = pages.find((p) => !p.url().includes("popup"));
  if (!main) throw new Error(`no main window among ${pages.map((p) => p.url()).join(", ")}`);
  await main.evaluate((h) => {
    location.hash = `#${h}`;
  }, page);
  await main.waitForTimeout(1200);
  await main.evaluate((s) => {
    // The page body scrolls inside the main panel, not the document.
    const scrollers = [...document.querySelectorAll("*")].filter((el) => {
      const st = getComputedStyle(el);
      return /(auto|scroll)/.test(st.overflowY) && el.scrollHeight > el.clientHeight + 4;
    });
    for (const el of scrollers) el.scrollTop = s === "bottom" ? el.scrollHeight : s === "top" ? 0 : Number(s);
  }, scroll);
  await main.waitForTimeout(400);
  console.log(`ok ${page} ${scroll} (${await main.title()})`);
} finally {
  // Disconnect only; the WebView2 keeps running.
  await browser.close();
}
