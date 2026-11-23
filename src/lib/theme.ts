// Theme preference management: Dark, Light, or System (follows Windows / prefers-color-scheme).
// Synced with `<html data-theme="...">`, `localStorage` (pre-paint anti-flash cache), and Rust.

export type ThemeChoice = "dark" | "light" | "system";
export type ResolvedTheme = "dark" | "light";

export const THEME_STORAGE_KEY = "wren.theme";

export function normalizeThemePreference(raw: string | null | undefined): ThemeChoice {
  if (raw === "dark" || raw === "light") return raw;
  return "system";
}

export function toSettingsTheme(choice: ThemeChoice): "dark" | "light" | null {
  return choice === "system" ? null : choice;
}

export function resolveTheme(choice: ThemeChoice, prefersDark: boolean): ResolvedTheme {
  if (choice === "dark" || choice === "light") return choice;
  return prefersDark ? "dark" : "light";
}

export function systemPrefersDark(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function urlThemeOverride(): "dark" | "light" | null {
  if (typeof location === "undefined") return null;
  const p = new URLSearchParams(location.search).get("theme");
  return p === "dark" || p === "light" ? p : null;
}

export function readCachedTheme(): ThemeChoice {
  const fromUrl = urlThemeOverride();
  if (fromUrl) return fromUrl;
  try {
    return normalizeThemePreference(localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    return "system";
  }
}

export function applyDomTheme(choice: ThemeChoice, persistCache = true): ResolvedTheme {
  const resolved = resolveTheme(choice, systemPrefersDark());
  if (typeof document !== "undefined") {
    document.documentElement.dataset.theme = resolved;
  }
  if (persistCache && !urlThemeOverride()) {
    try {
      if (choice === "system") localStorage.removeItem(THEME_STORAGE_KEY);
      else localStorage.setItem(THEME_STORAGE_KEY, choice);
    } catch {
      /* storage unavailable */
    }
  }
  return resolved;
}
