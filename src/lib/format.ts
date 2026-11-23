// Small display helpers. British English, 24-hour clock.

export interface Part {
  value: string;
  unit: string;
}

/** Time saved as figure/unit pairs: [9 "h", 42 "min"], [18 "min"], [40 "s"], [0 "min"], [2 "hours"]. */
export function formatSaved(ms: number): Part[] {
  const totalMin = Math.floor(ms / 60_000);
  if (ms <= 0) return [{ value: "0", unit: "min" }];
  if (totalMin < 1) return [{ value: String(Math.round(ms / 1000)), unit: "s" }];
  if (totalMin < 60) return [{ value: String(totalMin), unit: "min" }];
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  if (!m) return [{ value: String(h), unit: h === 1 ? "hour" : "hours" }];
  return [
    { value: String(h), unit: "h" },
    { value: String(m), unit: "min" },
  ];
}

/** Compact, for captions: "2h 18m", "45m", "1h", "30s". */
export function formatSavedShort(ms: number): string {
  const totalMin = Math.floor(ms / 60_000);
  if (ms <= 0) return "0m";
  if (totalMin < 1) return `${Math.round(ms / 1000)}s`;
  if (totalMin < 60) return `${totalMin}m`;
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  return m ? `${h}h ${m}m` : `${h}h`;
}

/** The same as one line of text: "9 h 42 min". */
export function formatSavedText(ms: number): string {
  return formatSaved(ms)
    .map((p) => `${p.value} ${p.unit}`)
    .join(" ");
}

export function formatClock(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
}

/** 72 → "3 days", 36 → "36 hours", 24 → "1 day". */
export function retentionLabel(hours: number): string {
  if (hours > 0 && hours % 24 === 0) {
    const d = hours / 24;
    return d === 1 ? "1 day" : `${d} days`;
  }
  return hours === 1 ? "1 hour" : `${hours} hours`;
}

/** "Today", "Yesterday", or "Mon 2 Nov". */
export function dayLabel(iso: string, now = new Date()): string {
  const d = new Date(iso);
  const start = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((start(now) - start(d)) / 86_400_000);
  if (diff === 0) return "Today";
  if (diff === 1) return "Yesterday";
  return d.toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short" });
}

/** "Just now", "4 min ago", "2 h ago", else the clock time with the day. */
export function whenLabel(iso: string, now = Date.now()): string {
  const diff = now - new Date(iso).getTime();
  if (diff < 60_000) return "Just now";
  if (diff < 3600_000) return `${Math.floor(diff / 60_000)} min ago`;
  const day = dayLabel(iso, new Date(now));
  if (day === "Today") return formatClock(iso);
  return `${day}, ${formatClock(iso)}`;
}

/** 15 → "3 pm", 0 → "12 am". */
export function hourLabel(h: number): string {
  const suffix = h < 12 ? "am" : "pm";
  const twelve = h % 12 === 0 ? 12 : h % 12;
  return `${twelve} ${suffix}`;
}

export function formatNumber(n: number): string {
  return n.toLocaleString("en-GB");
}

const APP_NAMES: Record<string, string> = {
  "claude.exe": "Claude",
  "code.exe": "VS Code",
  "cursor.exe": "Cursor",
  "outlook.exe": "Outlook",
  "olk.exe": "Outlook",
  "windowsterminal.exe": "Terminal",
  "wt.exe": "Terminal",
  "msedge.exe": "Edge",
  "chrome.exe": "Chrome",
  "firefox.exe": "Firefox",
  "winword.exe": "Word",
  "excel.exe": "Excel",
  "slack.exe": "Slack",
  "discord.exe": "Discord",
  "notepad.exe": "Notepad",
  "explorer.exe": "File Explorer",
  "teams.exe": "Teams",
  "ms-teams.exe": "Teams",
};

/** Friendly app name from a process name ("claude.exe" → "Claude"). */
export function appName(process: string): string {
  if (!process) return "";
  const key = process.toLowerCase();
  if (APP_NAMES[key]) return APP_NAMES[key];
  const base = process.replace(/\.exe$/i, "");
  return base.charAt(0).toUpperCase() + base.slice(1);
}

/** "0:42", "12:05". */
export function formatTimer(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** ["Willow"] → "Willow"; ["Willow", "Glaido"] → "Willow and Glaido". */
export function joinNames(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** 10_485_760 → "10.0 MB", 512_000 → "500 KB". */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
