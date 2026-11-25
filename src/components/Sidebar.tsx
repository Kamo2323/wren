import type { CSSProperties, ReactNode } from "react";
import type { EngineStatus, SetupStatus, UpdateStatus } from "../lib/types";
import { BookA, ChartColumn, Download, History, House, RotateCw, Settings, Users, WrenTile } from "./Icon";

export type PageId = "home" | "meetings" | "history" | "dictionary" | "stats" | "settings" | "setup";

const MAIN: Array<{ id: PageId; label: string; icon: ReactNode }> = [
  { id: "home", label: "Home", icon: <House size={18} strokeWidth={1.9} /> },
  { id: "meetings", label: "Meetings", icon: <Users size={18} strokeWidth={1.9} /> },
  { id: "history", label: "History", icon: <History size={18} strokeWidth={1.9} /> },
  { id: "dictionary", label: "Dictionary", icon: <BookA size={18} strokeWidth={1.9} /> },
  { id: "stats", label: "Stats", icon: <ChartColumn size={18} strokeWidth={1.9} /> },
];

function NavItem({
  id,
  label,
  icon,
  active,
  onSelect,
  live,
}: {
  id: PageId;
  label: string;
  icon: ReactNode;
  active: boolean;
  onSelect: (p: PageId) => void;
  live?: boolean;
}) {
  return (
    <button
      type="button"
      data-page={id}
      className={`nav-item ${active ? "is-active" : ""}`}
      aria-current={active ? "page" : undefined}
      onClick={() => onSelect(id)}
    >
      <span className="nav-item__icon">{icon}</span>
      {label}
      {live && <span className="nav-item__live" title="Recording meeting notes" aria-label="Recording" />}
    </button>
  );
}

/** Non-intrusive sidebar button shown only when an update can be installed or is in progress. */
function UpdatePill({ update, onInstall }: { update: UpdateStatus; onInstall: () => void }) {
  if (update.state === "available") {
    const tip = update.notes
      ? `Wren v${update.version} is available.\n\n${update.notes}\n\nClick to download and install.`
      : `Wren v${update.version} is available. Click to download and install.`;
    return (
      <button type="button" className="update-pill update-pill--available" onClick={onInstall} title={tip}>
        <span className="update-pill__icon">
          <Download size={14} strokeWidth={2.2} />
        </span>
        <span className="update-pill__text">Update · v{update.version}</span>
        <span className="update-pill__dot" aria-hidden="true" />
      </button>
    );
  }
  if (update.state === "downloading") {
    const pct = update.percent ?? null;
    return (
      <button
        type="button"
        className="update-pill update-pill--busy"
        disabled
        aria-label={`Downloading Wren v${update.version}`}
        style={{ "--update-pct": `${pct ?? 100}%` } as CSSProperties}
      >
        <span className={`update-pill__fill ${pct == null ? "is-indeterminate" : ""}`} aria-hidden="true" />
        <span className="update-pill__icon">
          <span className="spinner spinner--indigo" aria-hidden="true" />
        </span>
        <span className="update-pill__text num">{pct != null ? `Updating · ${pct}%` : "Downloading…"}</span>
      </button>
    );
  }
  if (update.state === "installing") {
    return (
      <button type="button" className="update-pill update-pill--busy" disabled style={{ "--update-pct": "100%" } as CSSProperties}>
        <span className="update-pill__fill" aria-hidden="true" />
        <span className="update-pill__icon">
          <span className="spinner spinner--indigo" aria-hidden="true" />
        </span>
        <span className="update-pill__text">Restarting…</span>
      </button>
    );
  }
  if (update.state === "error" && update.version) {
    return (
      <button type="button" className="update-pill update-pill--error" onClick={onInstall} title={`${update.message} Click to try again.`}>
        <span className="update-pill__icon">
          <RotateCw size={14} strokeWidth={2.2} />
        </span>
        <span className="update-pill__text">Retry update</span>
      </button>
    );
  }
  return null;
}

/** Speech engine status in plain words, for the footer dot and the Settings row. */
export function engineWords(s: EngineStatus): { tone: "ok" | "busy" | "warn" | "bad"; text: string; detail?: string } {
  switch (s.state) {
    case "ready":
      if (s.device === "cpu") {
        return { tone: "warn", text: "Ready · slower", detail: "Running on processor, so speech runs slower" };
      }
      if (s.device === "mlx") {
        return { tone: "ok", text: "Ready", detail: "Running on Apple Silicon" };
      }
      return { tone: "ok", text: "Ready", detail: "Running on your graphics card" };
    case "warming":
      return { tone: "busy", text: "Warming up" };
    case "starting":
      return { tone: "busy", text: "Starting" };
    case "failed":
      return { tone: "bad", text: "Speech stopped", detail: s.reason || "The speech engine stopped" };
    case "needsSetup":
      return { tone: "warn", text: "Set up speech", detail: s.reason || "Speech recognition isn't set up yet" };
    default:
      return { tone: "busy", text: "Connecting" };
  }
}

export function Sidebar({
  page,
  onSelect,
  engine,
  onOpenEngine,
  update,
  onInstallUpdate,
  meetingLive = false,
  setup,
}: {
  page: PageId;
  onSelect: (p: PageId) => void;
  engine: EngineStatus;
  onOpenEngine: () => void;
  update: UpdateStatus;
  onInstallUpdate: () => void;
  /** A meeting is recording: a red dot on the Meetings item. */
  meetingLive?: boolean;
  /** First-run speech setup: while it runs, the footer shows its progress instead of the engine. */
  setup?: SetupStatus;
}) {
  const status =
    setup?.state === "running"
      ? { tone: "busy" as const, text: `Setting up · ${Math.round(setup.fraction * 100)}%`, detail: setup.stepLabel }
      : setup?.state === "starting"
        ? { tone: "busy" as const, text: "Starting speech", detail: "The speech engine is warming up" }
        : engineWords(engine);
  const actionable = status.tone === "warn" || status.tone === "bad" || setup?.state === "running" || setup?.state === "starting";
  return (
    <aside className="sidebar" aria-label="Wren sidebar">
      <div className="sidebar__brand">
        <WrenTile size={32} />
        <span className="sidebar__name">Wren</span>
      </div>
      <nav className="sidebar__nav" aria-label="Pages">
        {MAIN.map((item) => (
          <NavItem key={item.id} {...item} active={page === item.id} onSelect={onSelect} live={item.id === "meetings" && meetingLive} />
        ))}
      </nav>
      <div className="sidebar__foot">
        <UpdatePill update={update} onInstall={onInstallUpdate} />
        <NavItem id="settings" label="Settings" icon={<Settings size={18} strokeWidth={1.9} />} active={page === "settings"} onSelect={onSelect} />
        {actionable ? (
          <button
            type="button"
            className={`engine engine--${status.tone} engine--action`}
            title={`${status.detail ?? status.text}. ${engine.state === "needsSetup" || setup?.state === "running" ? "Open speech setup." : "Open the speech settings."}`}
            onClick={onOpenEngine}
          >
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
