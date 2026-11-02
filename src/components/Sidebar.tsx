import type { ReactNode } from "react";
import type { EngineStatus } from "../lib/types";
import { BookA, ChartColumn, History, House, Settings, WrenTile } from "./Icon";

export type PageId = "home" | "history" | "dictionary" | "stats" | "settings";

const MAIN: Array<{ id: PageId; label: string; icon: ReactNode }> = [
  { id: "home", label: "Home", icon: <House size={19} strokeWidth={1.9} /> },
  { id: "history", label: "History", icon: <History size={19} strokeWidth={1.9} /> },
  { id: "dictionary", label: "Dictionary", icon: <BookA size={19} strokeWidth={1.9} /> },
  { id: "stats", label: "Stats", icon: <ChartColumn size={19} strokeWidth={1.9} /> },
];

function NavItem({ id, label, icon, active, onSelect }: { id: PageId; label: string; icon: ReactNode; active: boolean; onSelect: (p: PageId) => void }) {
  return (
    <button type="button" className={`nav-item ${active ? "is-active" : ""}`} aria-current={active ? "page" : undefined} onClick={() => onSelect(id)}>
      <span className="nav-item__icon">{icon}</span>
      {label}
    </button>
  );
}

/** Speech engine status in plain words, for the footer dot and the Settings row. */
export function engineWords(s: EngineStatus): { tone: "ok" | "busy" | "warn" | "bad"; text: string; detail?: string } {
  switch (s.state) {
    case "ready":
      return s.device === "cpu"
        ? { tone: "warn", text: "Ready · slower", detail: "The graphics card isn't available, so speech runs slower" }
        : { tone: "ok", text: "Ready", detail: "Running on your graphics card" };
    case "warming":
      return { tone: "busy", text: "Warming up" };
    case "starting":
      return { tone: "busy", text: "Starting" };
    case "failed":
      return { tone: "bad", text: "Speech stopped", detail: s.reason || "The speech engine stopped" };
    default:
      return { tone: "busy", text: "Connecting" };
  }
}

export function Sidebar({
  page,
  onSelect,
  engine,
  onOpenEngine,
}: {
  page: PageId;
  onSelect: (p: PageId) => void;
  engine: EngineStatus;
  /** Open Settings at the speech engine (offered when something is wrong). */
  onOpenEngine: () => void;
}) {
  const status = engineWords(engine);
  const actionable = status.tone === "warn" || status.tone === "bad";
  return (
    <aside className="sidebar">
      <div className="sidebar__brand">
        <WrenTile size={30} />
        <span className="sidebar__name">Wren</span>
      </div>
      <nav className="sidebar__nav" aria-label="Pages">
        {MAIN.map((item) => (
          <NavItem key={item.id} {...item} active={page === item.id} onSelect={onSelect} />
        ))}
      </nav>
      <div className="sidebar__foot">
        <NavItem id="settings" label="Settings" icon={<Settings size={19} strokeWidth={1.9} />} active={page === "settings"} onSelect={onSelect} />
        {actionable ? (
          <button type="button" className={`engine engine--${status.tone} engine--action`} title={`${status.detail ?? status.text}. Open the speech settings.`} onClick={onOpenEngine}>
            <span className="engine__dot" />
            <span className="engine__text">{status.text}</span>
          </button>
        ) : (
          <div className={`engine engine--${status.tone}`} title={status.detail}>
            <span className="engine__dot" />
            <span className="engine__text">{status.text}</span>
          </div>
        )}
      </div>
    </aside>
  );
}
