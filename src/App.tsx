// Main window shell: sidebar on the canvas, one white content panel, toasts.
// Home is built here (Task 6); History, Dictionary, Stats and Settings arrive with Task 7.
import { useEffect, useState } from "react";
import type { EngineStatus } from "./lib/types";
import { engineStatus, getSettings, onEngineStatus, subscribe } from "./lib/api";
import { Sidebar, type PageId } from "./components/Sidebar";
import { ToastProvider } from "./components/Toast";
import Home from "./pages/Home";

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

function ComingSoon({ page }: { page: PageId }) {
  return (
    <div className="page">
      <h1 className="page__title">{TITLES[page]}</h1>
      <p className="page__lede">This page is on its way.</p>
    </div>
  );
}

export default function App() {
  const [page, setPage] = useState<PageId>(initialPage);
  const [engine, setEngine] = useState<EngineStatus>({ state: "unknown" });
  const [typingWpm, setTypingWpm] = useState<number | undefined>(undefined); // Home falls back to the constants default

  useEffect(() => {
    void engineStatus().then(setEngine).catch(() => setEngine({ state: "unknown" }));
    void getSettings()
      .then((s) => setTypingWpm(s.typingWpm))
      .catch(() => {});
    return subscribe(onEngineStatus(setEngine));
  }, []);

  useEffect(() => {
    if (location.hash !== `#${page}`) history.replaceState(null, "", `#${page}`);
    document.title = page === "home" ? "Wren" : `${TITLES[page]} · Wren`;
  }, [page]);

  return (
    <ToastProvider>
      <div className="shell">
        <Sidebar page={page} onSelect={setPage} engine={engine} />
        <main className="panel" key={page}>
          {page === "home" ? <Home typingWpm={typingWpm} /> : <ComingSoon page={page} />}
        </main>
      </div>
    </ToastProvider>
  );
}
