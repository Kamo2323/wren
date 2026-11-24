// Launch the installed Microsoft Edge for screenshots (never downloads a browser).
// `channel: "msedge"` first; if Edge's usual Application folder is missing (it can be mid-update),
// fall back to the newest msedge.exe under EdgeCore, then to an already cached Playwright Chromium.
import { chromium } from "playwright-core";
import { existsSync, readdirSync } from "node:fs";
import { join } from "node:path";

function edgeCoreCandidates() {
  const roots = [
    join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Microsoft", "EdgeCore"),
    join(process.env.ProgramFiles ?? "C:\\Program Files", "Microsoft", "EdgeCore"),
  ];
  const found = [];
  for (const root of roots) {
    if (!existsSync(root)) continue;
    for (const v of readdirSync(root)) {
      const exe = join(root, v, "msedge.exe");
      if (existsSync(exe)) found.push({ v, exe });
    }
  }
  const key = (v) => v.split(".").map((n) => n.padStart(6, "0")).join(".");
  return found.sort((a, b) => (key(b.v) > key(a.v) ? 1 : -1)).map((f) => f.exe);
}

function cachedChromium() {
  const root = join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  if (!existsSync(root)) return [];
  return readdirSync(root)
    .filter((d) => /^chromium-\d+$/.test(d))
    .sort()
    .reverse()
    .map((d) => join(root, d, "chrome-win64", "chrome.exe"))
    .filter(existsSync);
}

function macBrowserCandidates() {
  if (process.platform !== "darwin") return [];
  return [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
  ].filter(existsSync);
}

export async function launchEdge() {
  if (process.platform === "darwin") {
    for (const exe of macBrowserCandidates()) {
      try {
        return await chromium.launch({ executablePath: exe });
      } catch {
        /* try next */
      }
    }
  }
  try {
    return await chromium.launch({ channel: "msedge" });
  } catch (err) {
    for (const exe of [...edgeCoreCandidates(), ...cachedChromium()]) {
      try {
        const b = await chromium.launch({ executablePath: exe });
        console.warn(`(channel msedge unavailable; using ${exe})`);
        return b;
      } catch {
        /* try next */
      }
    }
    throw err;
  }
}
