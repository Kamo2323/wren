// Main window shell: sidebar on the canvas, one white content panel, toasts, and the app-wide banner
// when another dictation app (Willow, Glaido) is running.
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { EngineStatus, Settings as SettingsData, UpdateStatus } from "./lib/types";
import {
  engineStatus,
  getSettings,
  mainReady,
  onEngineStatus,
  onOpenUpdates,
  onUpdateStatus,
  otherAppsRunning,
  subscribe,
  updateCheck,
  updateInstall,
  updateStatus,
} from "./lib/api";
import { Sidebar, type PageId } from "./components/Sidebar";
import { ToastProvider, useToast } from "./components/Toast";
import { OthersBanner } from "./components/Banner";
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

function AppShell() {
  const [page, setPage] = useState<PageId>(initialPage);
  const [engine, setEngine] = useState<EngineStatus>({ state: "unknown" });
  const [update, setUpdate] = useState<UpdateStatus>({ state: "idle" });
  const [typingWpm, setTypingWpm] = useState<number | undefined>(undefined); // pages fall back to the constants default
  const [others, setOthers] = useState<string[]>([]);
  const [bannerHiddenFor, setBannerHiddenFor] = useState<string | null>(null);
  const [settled, setSettled] = useState(false);
  const [settingsFocus, setSettingsFocus] = useState<{ group: "engine" | "updates"; at: number } | null>(null);
  const updateRef = useRef<UpdateStatus>(update);
  updateRef.current = update;
  const toast = useToast();

  useEffect(() => {
    const t = window.setTimeout(() => setSettled(true), SETTLE_MS);
    // Home says when its data is on screen; any other first page is ready once it has painted.
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
      .then((s) => setTypingWpm(s.typingWpm))
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

  // Other dictation apps: checked at startup and whenever the window comes back to the front.
  useEffect(() => {
    const check = () =>
      void otherAppsRunning()
        .then(setOthers)
        .catch(() => setOthers([]));
    check();
    window.addEventListener("focus", check);
    return () => window.removeEventListener("focus", check);
  }, []);

  // follow #page links (and the back button in a browser)
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

  // Only the page remounts on navigation; the panel and the banner stay, so the banner doesn't
  // re-animate or get announced again. The panel's scroll goes back to the top instead. Once the shell
  // is settled, the remounted page shows at once (no entrance replay: see .shell.is-settled).
  const panelRef = useRef<HTMLElement>(null);
  useEffect(() => {
    panelRef.current?.scrollTo(0, 0);
  }, [page]);

  const onSaved = useCallback((s: SettingsData) => setTypingWpm(s.typingWpm), []);
  const select = useCallback((p: PageId) => {
    setSettingsFocus(null);
    setPage(p);
  }, []);
  const openEngine = useCallback(() => {
    setPage("settings");
    setSettingsFocus({ group: "engine", at: Date.now() });
  }, []);

  // Hiding the banner lasts until the set of running apps changes.
  const othersKey = others.join(",");
  const showBanner = others.length > 0 && bannerHiddenFor !== othersKey;

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
        {showBanner && (
          <OthersBanner
            names={others}
            onDismiss={() => setBannerHiddenFor(othersKey)}
            onMore={page === "settings" ? undefined : () => select("settings")}
          />
        )}
        <Fragment key={page}>
          {page === "home" && <Home typingWpm={typingWpm} />}
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
            />
          )}
        </Fragment>
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
