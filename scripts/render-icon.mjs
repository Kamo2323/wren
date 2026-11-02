// Renders scripts/assets/wren-icon.svg to a transparent PNG with Edge (no browser download).
// Usage: node scripts/render-icon.mjs [out.png] [size]
// Then: npx tauri icon <out.png>   (regenerates src-tauri/icons)
import { launchEdge } from "./lib/browser.mjs";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const out = process.argv[2] ?? resolve(here, "assets", "wren-icon-1024.png");
const size = Number(process.argv[3] ?? 1024);
const svg = readFileSync(resolve(here, "assets", "wren-icon.svg"), "utf8").replace(
  'width="1024" height="1024"',
  `width="${size}" height="${size}"`,
);

const browser = await launchEdge();
const page = await browser.newPage({ viewport: { width: size, height: size } });
await page.setContent(`<html><body style="margin:0;background:transparent">${svg}</body></html>`);
await page.screenshot({ path: out, omitBackground: true });
await browser.close();
console.log(`wrote ${out} (${size}px)`);
