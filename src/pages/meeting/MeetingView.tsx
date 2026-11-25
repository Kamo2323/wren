// One meeting: header (title, timer, controls), then My notes / Transcript / Enhanced (spec §6.3).
// Live updates arrive as `meeting://event` (segments, speaking, status) and `meeting://note` (streaming).
import { useCallback, useEffect, useRef, useState } from "react";
import { subscribe } from "../../lib/api";
import { getSettings } from "../../lib/api";
import {
  claudeKeyStatus,
  errorText,
  meetingActive,
  meetingEnhance,
  meetingGenerating,
  meetingGet,
  onMeetingEvent,
  saveNotesDraft,
  setNotesFlush,
  onNoteEvent,
  type ActiveMeeting,
  type Length,
  type MeetingDetail,
  type NoteErrorKind,
} from "../../lib/meetings";
import { Tabs } from "../../components/Tabs";
import { useToast } from "../../components/Toast";
import { NotebookPen, Sparkles, AudioLines } from "../../components/Icon";
import { MeetingHeader } from "./MeetingHeader";
import { TranscriptTab } from "./TranscriptTab";
import { NotesTab } from "./NotesTab";
import { EnhancedTab, type NoteError } from "./EnhancedTab";

type TabId = "notes" | "transcript" | "enhanced";

export default function MeetingView({ id, onBack, onOpenSettings }: { id: string; onBack: () => void; onOpenSettings: () => void }) {
  const toast = useToast();
  const [detail, setDetail] = useState<MeetingDetail | null>(null);
  const [missing, setMissing] = useState(false);
  const [active, setActive] = useState<ActiveMeeting | null>(null);
  const [tab, setTab] = useState<TabId | null>(null);
  const [speaking, setSpeaking] = useState({ me: false, them: false });
  const [meName, setMeName] = useState("Me");
  const [hasKey, setHasKey] = useState(true);
  const [streams, setStreams] = useState<Partial<Record<Length, string>>>({});
  const [writing, setWriting] = useState<Length[]>([]);
  const [errors, setErrors] = useState<Partial<Record<Length, NoteError>>>({});
  const wasLive = useRef(false);
  /** Set by My notes while it is open: pushes any pending edit into the save queue. */
  const flushNotes = useRef<(() => Promise<void>) | null>(null);
  /** Lengths whose run already finished or failed: a late "still generating" answer can't revive them. */
  const settled = useRef(new Set<Length>());

  /** My notes saved: keep the latest text here (so reopening the tab never loads an old copy) and mark
   *  written notes as changed, like the store does. */
  const onNotesSaved = useCallback((json: string) => {
    setDetail((d) =>
      d && d.meeting.myNotes !== json
        ? { ...d, meeting: { ...d.meeting, myNotes: json }, notes: d.notes.map((n) => ({ ...n, stale: true })) }
        : d,
    );
  }, []);
  const saveNotes = useCallback(async () => {
    const json = await saveNotesDraft(id);
    if (json !== undefined) onNotesSaved(json);
  }, [id, onNotesSaved]);

  const beforeStop = useCallback(() => flushNotes.current?.() ?? saveNotes(), [saveNotes]);
  // Quit (the "Stop and quit" dialog) saves a pending edit first, like Stop does.
  useEffect(() => {
    setNotesFlush({ id, flush: beforeStop });
    return () => setNotesFlush(null);
  }, [id, beforeStop]);

  const reload = useCallback(async () => {
    const d = await meetingGet(id).catch(() => null);
    if (!d) return setMissing(true);
    setDetail(d);
    return d;
  }, [id]);

  useEffect(() => {
    let alive = true;
    const listeners = [
      onMeetingEvent((e) => {
        switch (e.kind) {
          case "segment":
            if (e.segment.meetingId !== id) return;
            setDetail((d) => {
              if (!d) return d;
              const existing = d.segments.find((s) => s.id === e.segment.id);
              if (existing) {
                return {
                  ...d,
                  segments: d.segments.map((s) => s.id === e.segment.id ? e.segment : s),
                  notes: existing.muted === e.segment.muted ? d.notes : d.notes.map((n) => ({ ...n, stale: true })),
                };
              }
              const segments = [...d.segments, e.segment].sort((a, b) => a.startMs - b.startMs || a.id - b.id);
              return { ...d, segments };
            });
            return;
          case "speaking":
            if (e.meetingId === id) setSpeaking((s) => (s[e.source] === e.on ? s : { ...s, [e.source]: e.on }));
            return;
          case "status": {
            const live = e.active && e.active.id === id ? e.active : null;
            setActive(live);
            if (live) {
              setSpeaking((s) => (s.me === live.speakingMe && s.them === live.speakingThem ? s : { me: live.speakingMe, them: live.speakingThem }));
              setDetail((d) =>
                d && (d.meeting.status !== live.status || d.meeting.captureMode !== live.captureMode)
                  ? { ...d, meeting: { ...d.meeting, status: live.status, captureMode: live.captureMode } }
                  : d,
              );
            } else if (wasLive.current) {
              // Tray Stop can arrive while the webview's editor timer is throttled.
              void beforeStop().catch(() => {});
              setSpeaking({ me: false, them: false });
              void reload();
              setTab("enhanced");
            }
            wasLive.current = !!live;
            return;
          }
          case "participants":
            if (e.meetingId === id) setDetail((d) => (d ? { ...d, meeting: { ...d.meeting, participants: e.names } } : d));
            return;
          case "title":
            if (e.meetingId === id) setDetail((d) => (d ? { ...d, meeting: { ...d.meeting, title: e.title } } : d));
            return;
        }
      }),
      onNoteEvent((e) => {
        if (e.event === "started") {
          if (e.meetingId !== id) return;
          settled.current.delete(e.length);
          setErrors((x) => ({ ...x, [e.length]: undefined }));
          setWriting((w) => (w.includes(e.length) ? w : [...w, e.length]));
          return;
        }
        if (e.event === "done") {
          if (e.note.meetingId !== id) return;
          const len = e.note.length;
          settled.current.add(len);
          setDetail((d) => (d ? { ...d, notes: [...d.notes.filter((n) => n.length !== len), e.note] } : d));
          setStreams((s) => ({ ...s, [len]: undefined }));
          setWriting((w) => w.filter((x) => x !== len));
          setErrors((x) => ({ ...x, [len]: undefined }));
          return;
        }
        if (e.meetingId !== id) return;
        if (e.event === "delta") {
          setWriting((w) => (w.includes(e.length) ? w : [...w, e.length]));
          setStreams((s) => ({ ...s, [e.length]: (s[e.length] ?? "") + e.text }));
        } else {
          settled.current.add(e.length);
          setStreams((s) => ({ ...s, [e.length]: undefined }));
          setWriting((w) => w.filter((x) => x !== e.length));
          setErrors((x) => ({ ...x, [e.length]: { kind: e.kind as NoteErrorKind, message: e.message } }));
          if (e.kind === "no_key") setHasKey(false);
        }
      }),
    ];
    const off = subscribe(listeners);
    void (async () => {
      // Listen first, then ask what is being written: a run that starts or ends in between is never
      // missed, and the answer is merged with what the events already said (never replaces it).
      await Promise.all(listeners).catch(() => {});
      const [, a, gen] = await Promise.all([reload(), meetingActive().catch(() => null), meetingGenerating(id).catch(() => [])]);
      if (!alive) return;
      const live = a && a.id === id ? a : null;
      setActive(live);
      wasLive.current = !!live;
      if (live) setSpeaking({ me: live.speakingMe, them: live.speakingThem });
      setWriting((w) => [...w, ...gen.filter((l) => !w.includes(l))].filter((l) => !settled.current.has(l)));
      setTab((t) => t ?? (live ? "transcript" : "enhanced"));
    })();
    void getSettings().then((s) => setMeName(s.meetingName?.trim() || "Me")).catch(() => {});
    void claudeKeyStatus().then((k) => setHasKey(k.saved)).catch(() => {});
    return () => {
      alive = false;
      off();
    };
  }, [id, reload, beforeStop]);

  const generate = useCallback(
    async (len: Length, regenerate: boolean) => {
      setErrors((x) => ({ ...x, [len]: undefined }));
      setWriting((w) => (w.includes(len) ? w : [...w, len]));
      try {
        const r = await meetingEnhance(id, len, regenerate);
        // "running": one is already being written — keep showing it. "exists": nothing was sent; the
        // notes were written meanwhile (e.g. the automatic Medium), so load them.
        if (r === "exists") {
          setWriting((w) => w.filter((x) => x !== len));
          void reload();
        }
      } catch (e) {
        setWriting((w) => w.filter((x) => x !== len));
        toast.show(errorText(e), { tone: "danger" });
      }
    },
    [id, toast, reload],
  );

  if (missing) {
    return (
      <div className="page meeting-view">
        <button type="button" className="back-link" onClick={onBack}>
          ← Meetings
        </button>
        <p className="page__lede">That meeting no longer exists.</p>
      </div>
    );
  }
  if (!detail || !tab) return <div className="page meeting-view" aria-busy="true" />;

  const live = !!active;
  return (
    <div className="page meeting-view">
      <MeetingHeader
        meeting={detail.meeting}
        active={active}
        meName={meName}
        onBack={onBack}
        onChanged={(m) => setDetail((d) => (d ? { ...d, meeting: m } : d))}
        beforeStop={beforeStop}
      />
      <div className="meeting-tabs">
        <Tabs<TabId>
          label="Meeting views"
          idPrefix="meeting"
          value={tab}
          onChange={setTab}
          items={[
            { id: "notes", label: "My notes", icon: <NotebookPen size={14} strokeWidth={2} /> },
            { id: "transcript", label: "Transcript", icon: <AudioLines size={14} strokeWidth={2} /> },
            { id: "enhanced", label: "Enhanced", icon: <Sparkles size={14} strokeWidth={2} /> },
          ]}
        />
      </div>
      <div className="meeting-panel" role="tabpanel" id={`meeting-panel-${tab}`} aria-labelledby={`meeting-tab-${tab}`}>
        {tab === "notes" && (
          <NotesTab meetingId={id} initial={detail.meeting.myNotes} save={saveNotes} flushRef={flushNotes} />
        )}
        {tab === "transcript" && (
          <TranscriptTab
            meeting={detail.meeting}
            segments={detail.segments}
            meName={meName}
            live={live}
            speaking={speaking}
            onRenamed={() => void reload()}
          />
        )}
        {tab === "enhanced" && (
          <EnhancedTab
            notes={detail.notes}
            streams={streams}
            writing={writing}
            errors={errors}
            live={live}
            hasKey={hasKey}
            title={detail.meeting.title}
            startedAtUtc={detail.meeting.startedAtUtc}
            onGenerate={generate}
            onOpenSettings={onOpenSettings}
          />
        )}
      </div>
    </div>
  );
}
