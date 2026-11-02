// Main window shell: sidebar on the canvas, one white content panel, toasts, and the app-wide banner
// when another dictation app (Willow, Glaido) is running.
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import type { EngineStatus, Settings as SettingsData } from "./lib/types";
import { engineStatus, getSettings, onEngineStatus, otherAppsRunning, subscribe } from "./lib/api";
import { Sidebar, type PageId } from "./components/Sidebar";
import { ToastProvider } from "./components/Toast";
import { OthersBanner } from "./components/Banner";
import Home from "./pages/Home";
import History from "./pages/History";
import Dictionary from "./pages/Dictionary";
import Stats from "./pages/Stats";
import Settings from "./pages/Settings";

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

export default function App() {
  const [page, setPage] = useState<PageId>(initialPage);
  const [engine, setEngine] = useState<EngineStatus>({ state: "unknown" });
  const [typingWpm, setTypingWpm] = useState<number | undefined>(undefined); // pages fall back to the constants default
  const [others, setOthers] = useState<string[]>([]);
  const [bannerHiddenFor, setBannerHiddenFor] = useState<string | null>(null);

  useEffect(() => {
    void engineStatus().then(setEngine).catch(() => setEngine({ state: "unknown" }));
    void getSettings()
      .then((s) => setTypingWpm(s.typingWpm))
      .catch(() => {});
    return subscribe(onEngineStatus(setEngine));
  }, []);

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

  // Only the page remounts on navigation (fresh entry animation); the panel and the banner stay, so the
  // banner doesn't re-animate or get announced again. The panel's scroll goes back to the top instead.
  const panelRef = useRef<HTMLElement>(null);
  useEffect(() => {
    panelRef.current?.scrollTo(0, 0);
  }, [page]);

  const onSaved = useCallback((s: SettingsData) => setTypingWpm(s.typingWpm), []);

  // Hiding the banner lasts until the set of running apps changes.
  const othersKey = others.join(",");
  const showBanner = others.length > 0 && bannerHiddenFor !== othersKey;

  return (
    <ToastProvider>
      <div className="shell">
        <Sidebar page={page} onSelect={setPage} engine={engine} />
        <main className="panel" ref={panelRef}>
          {showBanner && (
            <OthersBanner
              names={others}
              onDismiss={() => setBannerHiddenFor(othersKey)}
              onMore={page === "settings" ? undefined : () => setPage("settings")}
            />
          )}
          <Fragment key={page}>
            {page === "home" && <Home typingWpm={typingWpm} />}
            {page === "history" && <History />}
            {page === "dictionary" && <Dictionary />}
            {page === "stats" && <Stats typingWpm={typingWpm} />}
            {page === "settings" && <Settings engine={engine} others={others} onSaved={onSaved} />}
          </Fragment>
        </main>
      </div>
    </ToastProvider>
  );
}
