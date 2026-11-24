// Meeting header: back link, editable title, recording status + timer, capture note, Pause/Stop, people.
import { useEffect, useRef, useState } from "react";
import {
  errorText,
  meetingDelete,
  meetingPause,
  meetingResume,
  meetingStop,
  meetingUpdate,
  type ActiveMeeting,
  type MeetingRow,
} from "../../lib/meetings";
import { captureNote, durationLabel, formatElapsed } from "../../lib/meetingText";
import { Button } from "../../components/Button";
import { useToast } from "../../components/Toast";
import { ChevronLeft, PenLine, Pause, Play, Square, Trash2 } from "../../components/Icon";

/** Elapsed time that keeps ticking between the once-a-second status events. */
function useLiveElapsed(active: ActiveMeeting | null): number {
  const base = useRef({ ms: 0, at: 0 });
  const [, tick] = useState(0);
  useEffect(() => {
    if (active) base.current = { ms: active.elapsedMs, at: performance.now() };
  }, [active]);
  useEffect(() => {
    if (!active || active.status !== "recording") return;
    const t = setInterval(() => tick((n) => n + 1), 500);
    return () => clearInterval(t);
  }, [active]);
  if (!active) return 0;
  return active.status === "recording" ? base.current.ms + (performance.now() - base.current.at) : base.current.ms;
}

export function MeetingHeader({
  meeting,
  active,
  meName,
  onBack,
  onChanged,
}: {
  meeting: MeetingRow;
  active: ActiveMeeting | null;
  meName: string;
  onBack: () => void;
  onChanged: (m: MeetingRow) => void;
}) {
  const toast = useToast();
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(meeting.title);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const elapsed = useLiveElapsed(active);
  useEffect(() => setTitle(meeting.title), [meeting.title]);

  async function saveTitle() {
    setEditing(false);
    const t = title.trim();
    if (!t || t === meeting.title) return setTitle(meeting.title);
    try {
      await meetingUpdate(meeting.id, { title: t });
      onChanged({ ...meeting, title: t, titleEdited: true });
    } catch (e) {
      setTitle(meeting.title);
      toast.show(errorText(e), { tone: "danger" });
    }
  }

  async function act(fn: () => Promise<unknown>) {
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      toast.show(errorText(e), { tone: "danger" });
    } finally {
      setBusy(false);
    }
  }

  const paused = active?.status === "paused";
  const startedAt = new Date(meeting.startedAtUtc);
  const when = startedAt.toLocaleString("en-GB", { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  const people = [...meeting.participants, meName];

  return (
    <header className="mh">
      <button type="button" className="back-link" onClick={onBack}>
        <ChevronLeft size={15} strokeWidth={2.2} aria-hidden="true" /> Meetings
      </button>
      <div className="mh-row">
        <div className="mh-title-wrap">
          {editing ? (
            <input
              className="mh-title-input"
              value={title}
              autoFocus
              aria-label="Meeting title"
              onChange={(e) => setTitle(e.target.value)}
              onBlur={() => void saveTitle()}
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveTitle();
                if (e.key === "Escape") {
                  setTitle(meeting.title);
                  setEditing(false);
                }
              }}
            />
          ) : (
            <h1 className="page__title mh-title">
              <button type="button" className="mh-title-btn" onClick={() => setEditing(true)} title="Rename this meeting">
                {meeting.title}
                <PenLine size={15} strokeWidth={2} aria-hidden="true" className="mh-title-pen" />
              </button>
            </h1>
          )}
          <div className="mh-sub num">
            {active ? (
              <>
                <span className={`mh-status ${paused ? "is-paused" : "is-rec"}`}>
                  <span className="rec-dot" aria-hidden="true" />
                  {paused ? "Paused" : "Recording"} <span className="mh-timer">{formatElapsed(elapsed)}</span>
                </span>
                <span className="mh-note">{captureNote(active.captureMode)}</span>
                {active.catchingUp && <span className="mh-note mh-note--warn">Catching up…</span>}
              </>
            ) : (
              <>
                <span>{when}</span>
                {meeting.durationMs > 0 && <span>· {durationLabel(meeting.durationMs)}</span>}
                {meeting.status === "interrupted" && <span className="chip chip--warn">Interrupted</span>}
              </>
            )}
          </div>
        </div>
        <div className="mh-actions">
          {active ? (
            <>
              <Button
                variant="secondary"
                icon={paused ? <Play size={14} strokeWidth={2.2} /> : <Pause size={14} strokeWidth={2.2} />}
                disabled={busy}
                onClick={() => void act(paused ? meetingResume : meetingPause)}
              >
                {paused ? "Resume" : "Pause"}
              </Button>
              <Button variant="danger" icon={<Square size={13} strokeWidth={2.4} />} disabled={busy} onClick={() => void act(meetingStop)}>
                Stop
              </Button>
            </>
          ) : confirmDelete ? (
            <div className="mh-confirm">
              <span>Delete this meeting, its transcript and notes?</span>
              <Button size="sm" variant="ghost" onClick={() => setConfirmDelete(false)}>
                Cancel
              </Button>
              <Button
                size="sm"
                variant="danger"
                onClick={() =>
                  void act(async () => {
                    await meetingDelete(meeting.id);
                    onBack();
                  })
                }
              >
                Delete
              </Button>
            </div>
          ) : (
            <Button variant="ghost" size="sm" icon={<Trash2 size={14} strokeWidth={2} />} onClick={() => setConfirmDelete(true)}>
              Delete
            </Button>
          )}
        </div>
      </div>
      {people.length > 0 && (
        <ul className="mh-people" aria-label="People">
          {people.map((p) => (
            <li key={p} className="mh-person">
              {p}
            </li>
          ))}
        </ul>
      )}
    </header>
  );
}
