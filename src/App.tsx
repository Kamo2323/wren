// Main window shell: soft-gradient sidebar, full-bleed stage with Dark/Light switcher, toasts,
// and the app-wide banner when another dictation app (Willow, Glaido) is running.
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { EngineStatus, Settings as SettingsData, UpdateStatus } from "./lib/types";
import {
  accessibilityStatus,
  engineStatus,
  getSettings,
  mainReady,
  onEngineStatus,
  onOpenUpdates,
  onUpdateStatus,
  openAccessibilitySettings,
  otherAppsRunning,
  saveSettings,
  setWindowTheme,
  subscribe,
  updateCheck,
  updateInstall,
  updateStatus,
} from "./lib/api";
import { useConstants } from "./lib/useConstants";
import {
  applyDomTheme,
  normalizeThemePreference,
  readCachedTheme,
  toSettingsTheme,
  urlThemeOverride,
  type ResolvedTheme,
  type ThemeChoice,
} from "./lib/theme";
import { Sidebar, type PageId } from "./components/Sidebar";
import { ToastProvider, useToast } from "./components/Toast";
import { AccessibilityBanner, OthersBanner } from "./components/Banner";
import { Moon, Sun } from "./components/Icon";
import Home from "./pages/Home";
import History from "./pages/History";
import Dictionary from "./pages/Dictionary";
import Stats from "./pages/Stats";
import Settings from "./pages/Settings";

/** The entrance choreography plays once per app session; after this, pages switch instantly. */
const SETTLE_MS = 1400;

const TITLES: Record<PageId, string> = {
  home: "Home",
  history: "History",
  dictionary: "Dictionary",
  stats: "Stats",
  settings: "Settings",
};

function initialPage(): PageId {
  const h = location.hash.replace("#", "") as PageId;
  return h in TITLES ? h : "home";
}

function todayHeading(): string {
  return new Date().toLocaleDateString("en-GB", {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
}

function AppShell() {
  const [page, setPage] = useState<PageId>(initialPage);
  const [engine, setEngine] = useState<EngineStatus>({ state: "unknown" });
  const [update, setUpdate] = useState<UpdateStatus>({ state: "idle" });
  const [typingWpm, setTypingWpm] = useState<number | undefined>(undefined);
  const [themeChoice, setThemeChoice] = useState<ThemeChoice>(() => readCachedTheme());
  const [resolvedTheme, setResolvedTheme] = useState<ResolvedTheme>(() => applyDomTheme(readCachedTheme(), false));
  const constants = useConstants();
  const [others, setOthers] = useState<string[]>([]);
  const [bannerHiddenFor, setBannerHiddenFor] = useState<string | null>(null);
  const [accessibilityOk, setAccessibilityOk] = useState(true);
  const [accessibilityDismissed, setAccessibilityDismissed] = useState(false);
  const [settled, setSettled] = useState(false);
  const [settingsFocus, setSettingsFocus] = useState<{ group: "engine" | "updates"; at: number } | null>(null);
  const updateRef = useRef<UpdateStatus>(update);
  updateRef.current = update;
  const settingsRef = useRef<SettingsData | null>(null);
  const toast = useToast();

  // Keep DOM + native Windows caption synced with themeChoice and OS theme changes.
  useEffect(() => {
    const resolved = applyDomTheme(themeChoice, true);
    setResolvedTheme(resolved);
    void setWindowTheme(toSettingsTheme(themeChoice)).catch(() => {});

    if (themeChoice !== "system" || !window.matchMedia) return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => {
      setResolvedTheme(applyDomTheme("system", false));
      void setWindowTheme(null).catch(() => {});
    };
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [themeChoice]);

  useEffect(() => {
    const t = window.setTimeout(() => setSettled(true), SETTLE_MS);
    if (initialPage() !== "home") requestAnimationFrame(() => requestAnimationFrame(mainReady));
    return () => window.clearTimeout(t);
  }, []);

  const checkForUpdate = useCallback(async () => {
    try {
      const next = await updateCheck();
      setUpdate(next);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e || "Couldn't check for updates.");
      setUpdate({ state: "error", message: msg, version: null });
    }
  }, []);

  const installUpdate = useCallback(async () => {
    try {
      await updateInstall();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e || "The update didn't finish.");
      toast.show(msg, { tone: "danger" });
    }
  }, [toast]);

  useEffect(() => {
    void engineStatus().then(setEngine).catch(() => setEngine({ state: "unknown" }));
    void updateStatus().then(setUpdate).catch(() => {});
    void getSettings()
      .then((s) => {
        settingsRef.current = s;
        setTypingWpm(s.typingWpm);
        if (!urlThemeOverride()) {
          setThemeChoice(normalizeThemePreference(s.theme));
        }
      })
      .catch(() => {});
    return subscribe([
      onEngineStatus(setEngine),
      onUpdateStatus(setUpdate),
      onOpenUpdates(() => {
        setPage("settings");
        setSettingsFocus({ group: "updates", at: Date.now() });
        const st = updateRef.current.state;
        if (st === "idle" || st === "upToDate" || st === "error") {
          void checkForUpdate();
        }
      }),
    ]);
  }, [checkForUpdate]);

  useEffect(() => {
    const check = () => {
      void otherAppsRunning()
        .then(setOthers)
        .catch(() => setOthers([]));
      if (constants.platform === "macos") {
        void accessibilityStatus()
          .then(setAccessibilityOk)
          .catch(() => setAccessibilityOk(true));
      }
    };
    check();
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, [constants.platform]);

  useEffect(() => {
    const onHash = () => {
      const h = location.hash.replace("#", "") as PageId;
      if (h in TITLES) setPage(h);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  useEffect(() => {
    if (location.hash !== `#${page}`) history.replaceState(null, "", `${location.pathname}${location.search}#${page}`);
    document.title = page === "home" ? "Wren" : `${TITLES[page]} · Wren`;
  }, [page]);

  const panelRef = useRef<HTMLElement>(null);
  useEffect(() => {
    panelRef.current?.scrollTo(0, 0);
  }, [page]);

  const onSaved = useCallback((s: SettingsData) => {
    settingsRef.current = s;
    setTypingWpm(s.typingWpm);
    setThemeChoice(normalizeThemePreference(s.theme));
  }, []);

  const switchTheme = useCallback(async (next: ThemeChoice) => {
    setThemeChoice(next);
    const stored = settingsRef.current ?? (await getSettings().catch(() => null));
    if (!stored) return;
    const updated: SettingsData = { ...stored, theme: toSettingsTheme(next) };
    settingsRef.current = updated;
    await saveSettings(updated).catch(() => {});
  }, []);

  const select = useCallback((p: PageId) => {
    setSettingsFocus(null);
    setPage(p);
  }, []);

  const openEngine = useCallback(() => {
    setPage("settings");
    setSettingsFocus({ group: "engine", at: Date.now() });
  }, []);

  const othersKey = others.join(",");
  const showBanner = others.length > 0 && bannerHiddenFor !== othersKey;
  const showAccessibilityBanner = constants.platform === "macos" && !accessibilityOk && !accessibilityDismissed;

  return (
    <div className={`shell ${settled ? "is-settled" : ""}`}>
      <Sidebar
        page={page}
        onSelect={select}
        engine={engine}
        onOpenEngine={openEngine}
        update={update}
        onInstallUpdate={() => void installUpdate()}
      />
      <main className="panel" ref={panelRef}>
        {showAccessibilityBanner && (
          <AccessibilityBanner
            onAllow={() => void openAccessibilitySettings().catch(() => {})}
            onDismiss={() => setAccessibilityDismissed(true)}
          />
        )}
        {showBanner && (
          <OthersBanner
            names={others}
            onDismiss={() => setBannerHiddenFor(othersKey)}
            onMore={page === "settings" ? undefined : () => select("settings")}
          />
        )}
        <div className="stage-inner">
          <div className="toolbar">
            <span className="toolbar__label">{page === "home" ? todayHeading() : ""}</span>
            <div className="segments theme-switch" role="group" aria-label="Appearance">
              <button
                type="button"
                className="segments__btn"
                data-theme-opt="dark"
                aria-pressed={resolvedTheme === "dark"}
                onClick={() => void switchTheme("dark")}
              >
                <Moon size={14} strokeWidth={2} aria-hidden="true" />
                Dark
              </button>
              <button
                type="button"
                className="segments__btn"
                data-theme-opt="light"
                aria-pressed={resolvedTheme === "light"}
                onClick={() => void switchTheme("light")}
              >
                <Sun size={14} strokeWidth={2} aria-hidden="true" />
                Light
              </button>
            </div>
          </div>

          <Fragment key={page}>
            {page === "home" && (
              <Home typingWpm={typingWpm} theme={resolvedTheme} onOpenHistory={() => select("history")} />
            )}
            {page === "history" && <History />}
            {page === "dictionary" && <Dictionary />}
            {page === "stats" && <Stats typingWpm={typingWpm} />}
            {page === "settings" && (
              <Settings
                engine={engine}
                others={others}
                onSaved={onSaved}
                focus={settingsFocus}
                update={update}
                onCheckUpdate={checkForUpdate}
                onInstallUpdate={installUpdate}
                themeChoice={themeChoice}
                onThemeChange={switchTheme}
              />
            )}
          </Fragment>
        </div>
      </main>
    </div>
  );
}

export default function App() {
  return (
    <ToastProvider>
      <AppShell />
    </ToastProvider>
  );
}
