// Real app icons for transcript cards.
// In Tauri on Windows, Rust extracts the real executable icon for any installed app (cached per process).
// Bundled PNG/SVG artwork provides instant paint and crisp icons in mock/offline mode.
import { useEffect, useState } from "react";
import { appIcon, inTauri } from "./api";
import PNG_ICONS from "./appIconPngs.json";

function svgUrl(svg: string): string {
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

const SVG_ICONS: Record<string, string> = {
  code: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#0065A9"/><path fill="#23A9F2" d="M22.8 6.4 13.1 15.2l-5-3.8-2.3 1.2v6.8l2.3 1.2 5-3.8 9.7 8.8 3.6-1.7V8.1l-3.6-1.7zm0 5.4v8.4l-5.7-4.2 5.7-4.2z"/></svg>',
  ),
  cursor: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#181A20"/><path fill="#FFFFFF" d="M16 6.8 24.4 11.6 16 16.4 7.6 11.6 16 6.8z"/><path fill="#A6B0C4" d="M16 16.4v9.6l8.4-4.8v-9.6L16 16.4z"/><path fill="#6B748A" d="M16 16.4v9.6l-8.4-4.8v-9.6L16 16.4z"/></svg>',
  ),
  t3code: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#3B82F6"/><stop offset="1" stop-color="#6366F1"/></linearGradient></defs><rect width="32" height="32" rx="7" fill="url(#g)"/><text x="16" y="21" text-anchor="middle" fill="#fff" font-family="Inter,Segoe UI,sans-serif" font-size="13" font-weight="700">T3</text></svg>',
  ),
  terminal: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#2B303B"/><rect x="3" y="6" width="26" height="20" rx="4" fill="#1B1F27"/><path d="M8.5 12.5 13 16l-4.5 3.5" stroke="#E5E9F0" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/><path d="M15.5 19.5h7" stroke="#88C0D0" stroke-width="2.2" stroke-linecap="round"/></svg>',
  ),
  msedge: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><defs><linearGradient id="e1" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#38BDF8"/><stop offset="1" stop-color="#2563EB"/></linearGradient><linearGradient id="e2" x1="0" y1="1" x2="1" y2="0"><stop offset="0" stop-color="#10B981"/><stop offset="1" stop-color="#38BDF8"/></linearGradient></defs><circle cx="16" cy="16" r="12.5" fill="url(#e1)"/><path d="M6 19.5c2.2 5.4 8.5 7.5 14 5.5 2.5-.9 4.5-2.6 5.6-4.7-2.6 1.2-6 1.1-8.5-.4-2.8-1.6-4.2-4.8-3.3-7.6.6-1.9 2.3-3.2 4.3-3.2 2.4 0 4.3 1.7 4.6 3.9H14c-.6 2.8 1.5 5.4 4.5 5.6-3.7 1.3-8.4.4-12.5-1.1z" fill="url(#e2)"/></svg>',
  ),
  chrome: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><circle cx="16" cy="16" r="12.5" fill="#EA4335"/><path d="M16 16h12.5a12.5 12.5 0 0 1-11.2 12.4L16 16z" fill="#FBBC05"/><path d="M16 16 17.3 28.4A12.5 12.5 0 0 1 5.2 9.8L16 16z" fill="#34A853"/><circle cx="16" cy="16" r="5.8" fill="#fff"/><circle cx="16" cy="16" r="4.3" fill="#4285F4"/></svg>',
  ),
  firefox: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><defs><linearGradient id="f" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#FF980E"/><stop offset="1" stop-color="#EA3868"/></linearGradient></defs><circle cx="16" cy="16" r="12.5" fill="url(#f)"/><circle cx="16.5" cy="16.5" r="6.2" fill="#5B4AE6"/></svg>',
  ),
  winword: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#185ABD"/><rect x="12" y="6" width="15" height="20" rx="2.5" fill="#2B7CD3"/><rect x="4" y="9" width="13" height="14" rx="2.5" fill="#103F91"/><path d="M6.8 12.5 8.6 19.5h1.5l1-4.3 1 4.3h1.5l1.8-7h-1.6l-1 4.6-1-4.6h-1.4l-1 4.6-1-4.6z" fill="#fff"/></svg>',
  ),
  excel: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#107C41"/><rect x="12" y="6" width="15" height="20" rx="2.5" fill="#21A366"/><rect x="4" y="9" width="13" height="14" rx="2.5" fill="#185C37"/><path d="m7.5 12.5 2.4 3.5-2.5 3.5h1.8l1.6-2.4 1.6 2.4h1.8l-2.5-3.5 2.4-3.5h-1.8l-1.5 2.3-1.5-2.3z" fill="#fff"/></svg>',
  ),
  discord: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#5865F2"/><path fill="#fff" d="M22.2 10.7a15.3 15.3 0 0 0-3.8-1.2l-.5 1a14 14 0 0 0-3.8 0l-.5-1a15.3 15.3 0 0 0-3.8 1.2C7.4 14.3 6.8 17.8 7.1 21.3a15.4 15.4 0 0 0 4.6 2.3l1-1.6a9.6 9.6 0 0 1-1.5-.7l.4-.3c2.9 1.3 6 1.3 8.8 0l.4.3c-.5.3-1 .5-1.5.7l1 1.6a15.4 15.4 0 0 0 4.6-2.3c.4-4-.6-7.5-2.7-10.6zM13 18.8c-.9 0-1.6-.8-1.6-1.8s.7-1.8 1.6-1.8 1.6.8 1.6 1.8-.7 1.8-1.6 1.8zm6 0c-.9 0-1.6-.8-1.6-1.8s.7-1.8 1.6-1.8 1.6.8 1.6 1.8-.7 1.8-1.6 1.8z"/></svg>',
  ),
  teams: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#4B53BC"/><rect x="4.5" y="9" width="13" height="14" rx="2.5" fill="#353B96"/><path d="M8 13h6v1.7h-2.1V19.5h-1.8v-4.8H8V13z" fill="#fff"/><circle cx="21.5" cy="12" r="2.5" fill="#7B83EB"/><path d="M18.5 16h5.5a2 2 0 0 1 2 2v3a4 4 0 0 1-7.5 1.8V16z" fill="#7B83EB"/></svg>',
  ),
  explorer: svgUrl(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><path d="M4 9.5A2.5 2.5 0 0 1 6.5 7h5.8l2.2 2.4h11A2.5 2.5 0 0 1 28 11.9v11.6a2.5 2.5 0 0 1-2.5 2.5h-19A2.5 2.5 0 0 1 4 23.5v-14z" fill="#F5B942"/><rect x="4" y="11.5" width="24" height="14.5" rx="2.5" fill="#FFD166"/><rect x="10" y="20" width="12" height="3" rx="1.5" fill="#2B6CB0"/></svg>',
  ),
};

const PROCESS_MAP: Record<string, string> = {
  "claude.exe": "png:claude",
  claude: "png:claude",
  "slack.exe": "png:slack",
  slack: "png:slack",
  "notepad.exe": "png:notepad",
  notepad: "png:notepad",
  "outlook.exe": "png:outlook",
  "olk.exe": "png:outlook",
  outlook: "png:outlook",
  "code.exe": "svg:code",
  "cursor.exe": "svg:cursor",
  "t3 code (beta).exe": "svg:t3code",
  "t3-code.exe": "svg:t3code",
  "t3code.exe": "svg:t3code",
  "windowsterminal.exe": "svg:terminal",
  "wt.exe": "svg:terminal",
  "msedge.exe": "svg:msedge",
  "chrome.exe": "svg:chrome",
  "firefox.exe": "svg:firefox",
  "winword.exe": "svg:winword",
  "excel.exe": "svg:excel",
  "discord.exe": "svg:discord",
  "teams.exe": "svg:teams",
  "ms-teams.exe": "svg:teams",
  "explorer.exe": "svg:explorer",
};

const pngTable = PNG_ICONS as Record<string, string>;

export function bundledAppIcon(processName: string): string | null {
  const key = processName.trim().toLowerCase();
  if (!key) return null;
  let ref = PROCESS_MAP[key];
  if (!ref) {
    if (key.includes("t3 code") || key.includes("t3-code")) ref = "svg:t3code";
    else if (key.includes("claude")) ref = "png:claude";
    else if (key.includes("slack")) ref = "png:slack";
    else if (key.includes("outlook")) ref = "png:outlook";
    else if (key.includes("notepad")) ref = "png:notepad";
  }
  if (!ref) return null;
  if (ref.startsWith("png:")) {
    const b64 = pngTable[ref.slice(4)];
    return b64 ? `data:image/png;base64,${b64}` : null;
  }
  return SVG_ICONS[ref.slice(4)] ?? null;
}

const nativeCache = new Map<string, string | null>();
const inflight = new Map<string, Promise<string | null>>();

/** Resolves a real Windows executable icon in Tauri (cached), falling back immediately to bundled artwork. */
export function useAppIcon(processName: string): string | null {
  const norm = processName.trim().toLowerCase();
  const fallback = bundledAppIcon(processName);
  const [src, setSrc] = useState<string | null>(() => {
    if (!norm) return null;
    if (nativeCache.has(norm)) return nativeCache.get(norm) ?? fallback;
    return fallback;
  });

  useEffect(() => {
    if (!norm) {
      setSrc(null);
      return;
    }
    if (nativeCache.has(norm)) {
      setSrc(nativeCache.get(norm) ?? fallback);
      return;
    }
    setSrc(fallback);
    if (!inTauri) return;

    let cancelled = false;
    let pending = inflight.get(norm);
    if (!pending) {
      pending = appIcon(processName)
        .catch(() => null)
        .then((url) => {
          nativeCache.set(norm, url);
          inflight.delete(norm);
          return url;
        });
      inflight.set(norm, pending);
    }
    void pending.then((url) => {
      if (!cancelled && url) setSrc(url);
    });
    return () => {
      cancelled = true;
    };
  }, [norm, processName, fallback]);

  return src;
}
