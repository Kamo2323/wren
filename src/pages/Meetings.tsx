// Meetings: day-grouped cards of past meetings and the button that starts one; opening a card shows the
// meeting view (My notes / Transcript / Enhanced). Spec: docs/superpowers/specs/2026-11-24-meetings-design.md §6.
import { useCallback, useEffect, useMemo, useState } from "react";
import { subscribe } from "../lib/api";
import { dayLabel } from "../lib/format";
import { useConstants } from "../lib/useConstants";
import {
  errorText,
  meetingActive,
  meetingList,
  meetingStart,
  onMeetingEvent,
  openSystemAudioSettings,
  systemAudioPermission,
  type ActiveMeeting,
  type MeetingRow,
} from "../lib/meetings";
import { durationLabel, participantsLabel } from "../lib/meetingText";
import { Button } from "../components/Button";
import { useToast } from "../components/Toast";
import { FIX_PERMISSIONS_HINT, FixPermissionsButton } from "../components/FixPermissions";
import { AudioLines, CircleAlert, Users } from "../components/Icon";
import MeetingView from "./meeting/MeetingView";

/** The meeting left open when the user switched pages: reopened on return. */
let lastOpen: string | null = null;

function timeOf(iso: string) {
  return new Date(iso).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
}

function MeetingCard({ m, live, onOpen }: { m: MeetingRow; live: boolean; onOpen: () => void }) {
  const people = participantsLabel(m.participants);
  return (
    <button type="button" className={`meeting-card ${live ? "is-live" : ""}`} onClick={onOpen}>
      <div className="meeting-card__top">
        <span className="meeting-card__title">{m.title}</span>
        {live && <span className="chip chip--live">● Recording</span>}
        {m.status === "interrupted" && <span className="chip chip--warn">Interrupted</span>}
        <span className="meeting-card__meta num">
          {timeOf(m.startedAtUtc)}
          {m.status !== "recording" && m.status !== "paused" && ` · ${durationLabel(m.durationMs)}`}
          {people && ` · ${people}`}
        </span>
      </div>
      {m.preview && <p className="meeting-card__preview">{m.preview}</p>}
    </button>
  );
}

function SystemAudioCard({ onFixed }: { onFixed: () => void }) {
  return (
    <div className="perm-card" role="status">
      <CircleAlert size={16} strokeWidth={2.1} aria-hidden="true" />
      <div>
        <strong>Allow Wren to hear your meetings.</strong> macOS needs the System Audio Recording permission so Wren can transcribe
        what others say in Teams. {FIX_PERMISSIONS_HINT}
      </div>
      <Button size="sm" variant="primary" onClick={() => void openSystemAudioSettings()}>
        Open System Settings
      </Button>
      <FixPermissionsButton onDone={onFixed} />
    </div>
  );
}

function MeetingList({ onOpen }: { onOpen: (id: string) => void }) {
  const toast = useToast();
  const constants = useConstants();
  const [rows, setRows] = useState<MeetingRow[] | null>(null);
  const [active, setActive] = useState<ActiveMeeting | null>(null);
  const [starting, setStarting] = useState(false);
  const [perm, setPerm] = useState<string>("granted");
  const [ready, setReady] = useState(false);

  const load = useCallback(() => {
    void Promise.all([
      meetingList().then(setRows).catch(() => setRows([])),
      meetingActive().then(setActive).catch(() => {}),
    ]).then(() => setReady(true));
  }, []);

  useEffect(() => {
    load();
    // Re-check the macOS permission when Wren comes back to the front (after System Settings).
    const checkPerm = () => {
      if (constants.platform === "macos") void systemAudioPermission().then(setPerm).catch(() => {});
    };
    checkPerm();
    window.addEventListener("focus", checkPerm);
    const off = subscribe(
      onMeetingEvent((e) => {
        if (e.kind === "status") {
          setActive(e.active);
          if (!e.active) load();
        } else if (e.kind === "title" || e.kind === "participants") load();
      }),
    );
    return () => {
      off();
      window.removeEventListener("focus", checkPerm);
    };
  }, [load, constants.platform]);

  const groups = useMemo(() => {
    const out: Array<{ label: string; rows: MeetingRow[] }> = [];
    const now = new Date();
    for (const r of rows ?? []) {
      const label = dayLabel(r.startedAtUtc, now);
      const g = out[out.length - 1];
      if (g && g.label === label) g.rows.push(r);
      else out.push({ label, rows: [r] });
    }
    return out;
  }, [rows]);

  async function start() {
    if (active) return onOpen(active.id);
    setStarting(true);
    try {
      const m = await meetingStart();
      onOpen(m.id);
    } catch (e) {
      toast.show(errorText(e), { tone: "danger" });
    } finally {
      setStarting(false);
    }
  }

  return (
    <div className="page page--meetings">
      <header className="page-head">
        <div>
          <h1 className="page__title">Meetings</h1>
          <p className="page__lede">
            Live transcripts of your Teams calls with who said what, your own notes, and clear notes written by Claude when the
            meeting ends.
          </p>
        </div>
        {ready && <Button
          variant="primary"
          className={`meetings-start ${active ? "is-live" : ""}`}
          icon={active ? <AudioLines size={16} strokeWidth={2.2} /> : <span className="rec-dot" aria-hidden="true" />}
          onClick={() => void start()}
          disabled={starting}
        >
          {active ? "Open current meeting" : starting ? "Starting…" : "Start meeting notes"}
        </Button>}
      </header>
      {constants.platform === "macos" && perm !== "granted" && <SystemAudioCard onFixed={() => void systemAudioPermission().then(setPerm).catch(() => {})} />}
      {rows && rows.length === 0 && (
        <div className="empty meetings-empty">
          <span className="tinted-icon" aria-hidden="true">
            <Users size={18} strokeWidth={2} />
          </span>
          <h2 className="empty__title">No meetings yet</h2>
          <p className="empty__text">
            Join your Teams call, then press <strong>Start meeting notes</strong>. Wren transcribes both sides live and writes up
            the notes when you stop.
          </p>
        </div>
      )}
      {groups.map((g) => (
        <section className="day" key={g.label}>
          <h2 className="day__label">{g.label}</h2>
          <div className="day__cards">
            {g.rows.map((m) => (
              <MeetingCard key={m.id} m={m} live={active?.id === m.id} onOpen={() => onOpen(m.id)} />
            ))}
          </div>
        </section>
      ))}
    </div>
  );
}

export default function Meetings({ onOpenSettings }: { onOpenSettings: () => void }) {
  const [openId, setOpenId] = useState<string | null>(lastOpen);
  const open = useCallback((id: string | null) => {
    lastOpen = id;
    setOpenId(id);
  }, []);
  if (openId) {
    return <MeetingView key={openId} id={openId} onBack={() => open(null)} onOpenSettings={onOpenSettings} />;
  }
  return <MeetingList onOpen={open} />;
}
