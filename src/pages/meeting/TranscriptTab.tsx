// Transcript: one speaker's consecutive lines grouped into a run, each name in a steady accent colour.
// While recording it follows the newest line unless the user has scrolled up ("Jump to live"); a
// pending row shows who is talking before their words arrive. Names can be fixed; lines replayed (24 h).
import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { errorText, meetingAudioClip, meetingRenameSpeaker, type MeetingRow, type SegmentRow } from "../../lib/meetings";
import { assignColours, clockAt, displayName, groupRuns, type Run } from "../../lib/meetingText";
import { useToast } from "../../components/Toast";
import { ArrowDown, Pause, Play } from "../../components/Icon";

const NOT_TRANSCRIBED = "[not transcribed]";
/** Below this confidence the name is shown greyed with "?" (spec §5.3). */
const LOW_CONFIDENCE = 0.6;
/** "At the bottom" if within this many pixels. */
const STICK_PX = 48;

type RenameMode = "all" | "run";

function RenamePopover({
  run,
  meName,
  onDone,
  onCancel,
}: {
  run: Run;
  meName: string;
  onDone: (to: string, mode: RenameMode) => void;
  onCancel: () => void;
}) {
  const current = displayName(run.lines[0], meName);
  const [mode, setMode] = useState<RenameMode | null>(null);
  const [value, setValue] = useState(run.speaker ?? "");
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && onCancel();
    document.addEventListener("mousedown", away);
    return () => document.removeEventListener("mousedown", away);
  }, [onCancel]);
  return (
    <div className="rename" ref={ref} role="dialog" aria-label={`Rename ${current}`}>
      {mode === null ? (
        <>
          <button type="button" className="rename__opt" autoFocus onClick={() => setMode("all")}>
            Rename all “{current}” lines
          </button>
          <button type="button" className="rename__opt" onClick={() => setMode("run")}>
            Rename just these lines
          </button>
        </>
      ) : (
        <input
          className="rename__input"
          autoFocus
          value={value}
          placeholder="Their name"
          aria-label="New name"
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && value.trim()) onDone(value.trim(), mode);
            if (e.key === "Escape") onCancel();
          }}
        />
      )}
    </div>
  );
}

const RunView = memo(function RunView({
  run,
  startedAtUtc,
  audioAvailable,
  meName,
  playing,
  onPlay,
  renaming,
  onRenameStart,
  onRenameDone,
  onRenameCancel,
  colour,
}: {
  run: Run;
  colour: number;
  startedAtUtc: string;
  audioAvailable: boolean;
  meName: string;
  /** The playing line's id, only when it is in this run (keeps other runs from re-rendering). */
  playing: number | null;
  onPlay: (s: SegmentRow) => void;
  renaming: boolean;
  onRenameStart: (key: string) => void;
  onRenameDone: (run: Run, to: string, mode: RenameMode) => void;
  onRenameCancel: () => void;
}) {
  const name = displayName(run.lines[0], meName);
  const unknown = run.source === "them" && !run.speaker;
  const unsure = run.source === "them" && !!run.speaker && !run.lines[0].renamed && run.lines[0].confidence < LOW_CONFIDENCE;
  const editable = run.source === "them";
  return (
    <div className="trun" data-colour={unknown ? "none" : colour} data-source={run.source}>
      <div className="trun__head">
        {editable ? (
          <button
            type="button"
            className={`trun__name ${unknown || unsure ? "is-unsure" : ""}`}
            onClick={() => onRenameStart(run.key)}
            title="Fix this name"
          >
            {name}
            {unsure && "?"}
          </button>
        ) : (
          <span className="trun__name">{name}</span>
        )}
        <span className="trun__time num">{clockAt(startedAtUtc, run.startMs)}</span>
        {renaming && <RenamePopover run={run} meName={meName} onDone={(to, mode) => onRenameDone(run, to, mode)} onCancel={onRenameCancel} />}
      </div>
      {run.lines.map((l) => {
        const failed = l.text === NOT_TRANSCRIBED;
        return (
          <p key={l.id} className={`tline ${l.muted ? "is-muted" : ""} ${failed ? "is-failed" : ""}`}>
            <span className="tline__text">{failed ? "Couldn't transcribe this part." : l.text}</span>
            {l.muted && <span className="tline__tag" title="You were muted in Teams. Left out of the notes.">muted</span>}
            {audioAvailable && !failed && (
              <button
                type="button"
                className={`tline__play ${playing === l.id ? "is-playing" : ""}`}
                aria-label={playing === l.id ? "Stop playing" : "Play this line"}
                title={playing === l.id ? "Stop" : "Play this line"}
                onClick={() => onPlay(l)}
              >
                {playing === l.id ? <Pause size={12} strokeWidth={2.4} /> : <Play size={12} strokeWidth={2.4} />}
              </button>
            )}
          </p>
        );
      })}
    </div>
  );
});

/** Render this much above and below the viewport. */
const OVERSCAN_PX = 900;

/** Height guess for a run not yet measured: name row plus wrapped lines (~95 chars per line). */
function estimateRun(r: Run): number {
  return 46 + r.lines.reduce((h, l) => h + 23 * Math.max(1, Math.ceil(l.text.length / 95)), 0);
}

/** A measured slot: reports its height to the transcript's ResizeObserver. */
function Slot({ k, ro, children }: { k: string; ro: { current: ResizeObserver | null }; children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    const obs = ro.current;
    if (!el || !obs) return;
    obs.observe(el);
    return () => obs.unobserve(el);
  }, [ro]);
  return (
    <div className="trun-slot" data-key={k} ref={ref}>
      {children}
    </div>
  );
}

export function TranscriptTab({
  meeting,
  segments,
  meName,
  live,
  speaking,
  onRenamed,
}: {
  meeting: MeetingRow;
  segments: SegmentRow[];
  meName: string;
  live: boolean;
  speaking: { me: boolean; them: boolean };
  onRenamed: () => void;
}) {
  const toast = useToast();
  const box = useRef<HTMLDivElement>(null);
  const [atBottom, setAtBottom] = useState(true);
  const stick = useRef(true);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [playing, setPlaying] = useState<number | null>(null);
  const audio = useRef<HTMLAudioElement | null>(null);
  const runs = useMemo(() => groupRuns(segments), [segments]);
  const colours = useMemo(
    () => assignColours(runs.filter((r) => r.source === "me" || r.speaker).map((r) => displayName(r.lines[0], meName)), meName),
    [runs, meName],
  );

  // Windowed rendering (plan Review Focus 4): only the runs near the viewport are in the DOM; the
  // rest are spacers sized from measured (or estimated) heights, so a 2-hour meeting scrolls smoothly.
  const heights = useRef(new Map<string, number>());
  const [measured, setMeasured] = useState(0);
  const [view, setView] = useState({ top: 0, height: 900 });
  // Created during the first render (not in an effect), so it exists before the first slots mount and
  // measure themselves; the effect only disconnects it.
  const ro = useRef<ResizeObserver | null>(null);
  const frame = useRef(0);
  if (ro.current === null && typeof ResizeObserver !== "undefined") {
    ro.current = new ResizeObserver((entries) => {
      let changed = false;
      for (const e of entries) {
        const el = e.target as HTMLElement;
        const k = el.dataset.key;
        if (!k) continue;
        const h = el.offsetHeight;
        if (h > 0 && heights.current.get(k) !== h) {
          heights.current.set(k, h);
          changed = true;
        }
      }
      if (changed && !frame.current)
        frame.current = requestAnimationFrame(() => {
          frame.current = 0;
          setMeasured((n) => n + 1);
        });
    });
  }
  useEffect(
    () => () => {
      cancelAnimationFrame(frame.current);
      ro.current?.disconnect();
    },
    [],
  );
  const tops = useMemo(() => {
    const t = [0];
    runs.forEach((r, i) => t.push(t[i] + (heights.current.get(r.key) ?? estimateRun(r))));
    return t;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runs, measured]);
  const total = tops[runs.length];
  const lo = view.top - OVERSCAN_PX;
  const hi = view.top + view.height + OVERSCAN_PX;
  let start = 0;
  while (start < runs.length && tops[start + 1] < lo) start++;
  let end = start;
  while (end < runs.length && tops[end] < hi) end++;

  // Follow the newest line while the user is at the bottom.
  useLayoutEffect(() => {
    const el = box.current;
    if (el && live && stick.current) el.scrollTop = el.scrollHeight;
  }, [runs, total, live, speaking]);
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    if (live) el.scrollTop = el.scrollHeight;
    setView({ top: el.scrollTop, height: el.clientHeight });
  }, [live]);

  const scrollFrame = useRef(0);
  function onScroll() {
    const el = box.current;
    if (!el) return;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < STICK_PX;
    stick.current = bottom;
    setAtBottom(bottom);
    if (!scrollFrame.current) {
      scrollFrame.current = requestAnimationFrame(() => {
        scrollFrame.current = 0;
        if (box.current) setView({ top: box.current.scrollTop, height: box.current.clientHeight });
      });
    }
  }

  const playingRef = useRef<number | null>(null);
  playingRef.current = playing;
  /** Bumped by every click and on leaving: a clip that arrives late for an older click never plays. */
  const playSeq = useRef(0);
  useEffect(
    () => () => {
      playSeq.current++;
      audio.current?.pause();
    },
    [],
  );
  const play = useCallback(async (s: SegmentRow) => {
    const mine = ++playSeq.current;
    audio.current?.pause();
    const wasThis = playingRef.current === s.id;
    setPlaying(null);
    if (wasThis) return;
    try {
      const url = await meetingAudioClip(meeting.id, s.source, s.startMs, s.endMs);
      if (mine !== playSeq.current) return;
      if (!url) return toast.show("This recording has been deleted (meeting audio is kept for 24 hours).");
      audio.current?.pause();
      const a = new Audio(url);
      audio.current = a;
      setPlaying(s.id);
      a.onended = () => setPlaying((p) => (p === s.id ? null : p));
      await a.play();
    } catch (e) {
      if (mine !== playSeq.current) return;
      setPlaying(null);
      toast.show(errorText(e), { tone: "danger" });
    }
  }, [meeting.id, toast]);
  const onPlay = useCallback((s: SegmentRow) => void play(s), [play]);
  const onRenameStart = useCallback((key: string) => setRenaming(key), []);
  const onRenameCancel = useCallback(() => setRenaming(null), []);

  const rename = useCallback(async (run: Run, to: string, mode: RenameMode) => {
    setRenaming(null);
    try {
      if (mode === "all") {
        await meetingRenameSpeaker(meeting.id, to, null, run.speaker);
      } else {
        for (const l of run.lines) await meetingRenameSpeaker(meeting.id, to, l.id, null);
      }
      onRenamed();
    } catch (e) {
      toast.show(errorText(e), { tone: "danger" });
    }
  }, [meeting.id, onRenamed, toast]);
  const onRenameDone = useCallback((run: Run, to: string, mode: RenameMode) => void rename(run, to, mode), [rename]);

  const empty = runs.length === 0 && !(live && (speaking.me || speaking.them));
  return (
    <div className="transcript-wrap">
      <div className="transcript" ref={box} onScroll={onScroll}>
        {empty && (
          <p className="transcript__empty">
            {live ? "Listening… lines appear a few seconds after someone finishes speaking." : "Nothing was transcribed in this meeting."}
          </p>
        )}
        <div style={{ height: tops[start] }} aria-hidden="true" />
        {runs.slice(start, end).map((r) => (
          <Slot key={r.key} k={r.key} ro={ro}>
            <RunView
              run={r}
              colour={colours.get(displayName(r.lines[0], meName)) ?? 0}
              startedAtUtc={meeting.startedAtUtc}
              audioAvailable={meeting.audioAvailable}
              meName={meName}
              playing={playing !== null && r.lines.some((l) => l.id === playing) ? playing : null}
              onPlay={onPlay}
              renaming={renaming === r.key}
              onRenameStart={onRenameStart}
              onRenameDone={onRenameDone}
              onRenameCancel={onRenameCancel}
            />
          </Slot>
        ))}
        <div style={{ height: total - tops[end] }} aria-hidden="true" />
        {live && speaking.them && (
          <div className="trun trun--pending" data-colour="none">
            <p className="tline tline--pending" aria-label="Someone is talking">
              <span className="trun__name is-unsure">Someone</span>
              <span className="dots" aria-hidden="true">
                <i />
                <i />
                <i />
              </span>
            </p>
          </div>
        )}
        {live && speaking.me && (
          <div className="trun trun--pending" data-colour={0} data-source="me">
            <p className="tline tline--pending" aria-label={`${meName} is talking`}>
              <span className="trun__name">{meName}</span>
              <span className="dots" aria-hidden="true">
                <i />
                <i />
                <i />
              </span>
            </p>
          </div>
        )}
      </div>
      {live && !atBottom && (
        <button
          type="button"
          className="jump-live"
          onClick={() => {
            const el = box.current;
            if (el) el.scrollTop = el.scrollHeight;
          }}
        >
          <ArrowDown size={14} strokeWidth={2.2} aria-hidden="true" /> Jump to live
        </button>
      )}
    </div>
  );
}
