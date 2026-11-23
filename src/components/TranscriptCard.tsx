// One dictation as a soft-tinted hairline card with real app icon, click-to-copy, Fix a word,
// Delete confirm, and optional inline WAV playback when a local recording exists.
import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import type { TranscriptionRow } from "../lib/types";
import { historyAudio } from "../lib/api";
import { useAppIcon } from "../lib/appIcons";
import { appName, whenLabel } from "../lib/format";
import { markMatches } from "../lib/text";
import { AppWindow, Check, Copy, Mic, Pause, PenLine, Play, Trash2, X } from "./Icon";
import { Button, IconButton } from "./Button";

export interface TranscriptCardProps {
  row: TranscriptionRow;
  onCopy: (row: TranscriptionRow) => Promise<boolean>;
  /** Rejects if the delete failed; the card then comes back. */
  onDelete: (row: TranscriptionRow) => Promise<void>;
  onFix: (row: TranscriptionRow, from: string, to: string) => Promise<void>;
  now?: number;
  /** Search words to mark in the app name and text (History search). */
  highlight?: string;
  /** A dictation that arrived since the page last looked: it rises in (others appear at once). */
  isNew?: boolean;
  /** Whether a local WAV recording exists on disk for inline playback. */
  hasAudio?: boolean;
  /** Controlled active playback id so only one recording plays at a time. */
  playingId?: string | null;
  onPlayToggle?: (id: string | null) => void;
}

type Token = { text: string; word: boolean; core: string; lead: string; trail: string };

/** Split text into words and the spaces between them; a word keeps its punctuation apart from its core. */
export function tokenise(text: string): Token[] {
  return text
    .split(/(\s+)/)
    .filter(Boolean)
    .map((part) => {
      if (/^\s+$/.test(part)) return { text: part, word: false, core: "", lead: "", trail: "" };
      const m = part.match(/^([^\p{L}\p{N}]*)(.*?)([^\p{L}\p{N}]*)$/u);
      const [lead, core, trail] = m ? [m[1], m[2], m[3]] : ["", part, ""];
      return { text: part, word: core.length > 0, core, lead, trail };
    });
}

function formatAudioClock(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) return "0:00";
  const s = Math.floor(sec);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function AppBadge({ processName }: { processName: string }) {
  const iconSrc = useAppIcon(processName);
  if (iconSrc) {
    return (
      <span className="app-icon" aria-hidden="true">
        <img src={iconSrc} alt="" width={25} height={25} />
      </span>
    );
  }
  return (
    <span className="app-icon app-icon--fallback" aria-hidden="true">
      <AppWindow size={15} strokeWidth={1.9} />
    </span>
  );
}

const COPIED_MS = 1600;

export function TranscriptCard({
  row,
  onCopy,
  onDelete,
  onFix,
  now,
  highlight,
  isNew,
  hasAudio,
  playingId,
  onPlayToggle,
}: TranscriptCardProps) {
  const [arrived] = useState(isNew === true);
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [fixing, setFixing] = useState(false);
  const [picked, setPicked] = useState<number | null>(null);
  const [replacement, setReplacement] = useState("");
  const [saving, setSaving] = useState(false);
  const [leaving, setLeaving] = useState(false);

  // Audio playback state (only active when this card is the active playingId)
  const audioOpen = hasAudio === true && playingId === row.id;
  const [audioPaused, setAudioPaused] = useState(true);
  const [audioTime, setAudioTime] = useState(0);
  const [audioDuration, setAudioDuration] = useState(0);
  const audioRef = useRef<HTMLAudioElement | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const timers = useRef<number[]>([]);

  useEffect(() => () => timers.current.forEach((t) => window.clearTimeout(t)), []);
  useEffect(() => {
    if (picked !== null) inputRef.current?.focus();
  }, [picked]);

  // Clean up audio whenever this card stops being the active player
  useEffect(() => {
    if (!audioOpen) {
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
      setAudioPaused(true);
      setAudioTime(0);
      setAudioDuration(0);
      return;
    }
    let cancelled = false;
    void historyAudio(row.id).then((url) => {
      if (cancelled) return;
      if (!url) {
        onPlayToggle?.(null);
        return;
      }
      const el = new Audio(url);
      audioRef.current = el;
      const sync = () => {
        setAudioPaused(el.paused);
        setAudioTime(el.currentTime || 0);
        if (Number.isFinite(el.duration)) setAudioDuration(el.duration);
      };
      const onEnd = () => {
        setAudioPaused(true);
        setAudioTime(el.duration || 0);
      };
      el.addEventListener("loadedmetadata", sync);
      el.addEventListener("timeupdate", sync);
      el.addEventListener("play", sync);
      el.addEventListener("pause", sync);
      el.addEventListener("ended", onEnd);
      void el.play().catch(() => setAudioPaused(true));
    });
    return () => {
      cancelled = true;
      if (audioRef.current) {
        audioRef.current.pause();
        audioRef.current = null;
      }
    };
  }, [audioOpen, row.id, onPlayToggle]);

  const tokens = fixing ? tokenise(row.finalText) : [];
  const handsFree = row.mode === "handsFree" || row.mode === "handsfree";
  const notPasted = row.pasteOutcome === "notPasted" || row.pasteOutcome === "not_pasted" || row.pasteOutcome === "failed";
  const label = appName(row.targetProcess) || "Wren";

  async function copy() {
    if (fixing) return;
    const ok = await onCopy(row);
    if (!ok) return;
    setCopied(true);
    timers.current.push(window.setTimeout(() => setCopied(false), COPIED_MS));
  }

  function stop(e: MouseEvent) {
    e.stopPropagation();
  }

  function togglePlay(e: MouseEvent) {
    e.stopPropagation();
    if (!audioOpen) {
      onPlayToggle?.(row.id);
      return;
    }
    const el = audioRef.current;
    if (!el) return;
    if (el.paused) void el.play().catch(() => {});
    else el.pause();
  }

  function startFix(e: MouseEvent) {
    e.stopPropagation();
    setConfirmDelete(false);
    setFixing(true);
    setPicked(null);
    setReplacement("");
  }

  function cancelFix() {
    setFixing(false);
    setPicked(null);
    setReplacement("");
  }

  async function saveFix() {
    if (picked === null) return;
    const from = tokens[picked].core;
    const to = replacement.trim();
    if (!to || to === from) return;
    setSaving(true);
    try {
      await onFix(row, from, to);
      cancelFix();
    } catch {
      // The page has already said it failed; stay in fix mode so nothing typed is lost.
    } finally {
      setSaving(false);
    }
  }

  async function del(e: MouseEvent) {
    e.stopPropagation();
    if (!confirmDelete) {
      setConfirmDelete(true);
      timers.current.push(window.setTimeout(() => setConfirmDelete(false), 3500));
      return;
    }
    if (audioOpen) onPlayToggle?.(null);
    setLeaving(true);
    timers.current.push(
      window.setTimeout(() => {
        onDelete(row).catch(() => {
          setLeaving(false);
          setConfirmDelete(false);
        });
      }, 180),
    );
  }

  function onKey(e: KeyboardEvent<HTMLDivElement>) {
    if (e.target !== e.currentTarget) return;
    if (fixing) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      void copy();
    }
  }

  function onInputKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Enter") {
      e.preventDefault();
      void saveFix();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelFix();
    }
  }

  const cls = ["tcard", arrived && "is-new", copied && "is-copied", fixing && "is-fixing", leaving && "is-leaving"]
    .filter(Boolean)
    .join(" ");

  const renderMarked = (text: string) =>
    highlight?.trim()
      ? markMatches(text, highlight).map((p, i) => (p.match ? <mark key={i}>{p.text}</mark> : <span key={i}>{p.text}</span>))
      : text;

  return (
    <article
      className={cls}
      role={fixing ? undefined : "button"}
      tabIndex={fixing ? -1 : 0}
      aria-label={fixing ? undefined : `Copy: ${row.finalText.slice(0, 80)}`}
      onClick={copy}
      onKeyDown={onKey}
    >
      <div className="tcard__row">
        <div className="tcard__main">
          <AppBadge processName={row.targetProcess} />
          <div className="tcard__body">
            <div className="tcard__meta">
              <span className="tcard__app">{renderMarked(label)}</span>
              <span className="tcard__sep" aria-hidden="true">·</span>
              <span>{whenLabel(row.createdAtUtc, now)}</span>
              <span className="tcard__sep" aria-hidden="true">·</span>
              <span className="num">
                {row.words} {row.words === 1 ? "word" : "words"}
              </span>
              {handsFree && <span className="chip chip--quiet">Hands-free</span>}
              {notPasted && <span className="chip chip--warn">Not pasted</span>}
            </div>

            {!fixing && (
              <p className={`tcard__text ${highlight?.trim() ? "tcard__text--full" : ""}`}>{renderMarked(row.finalText)}</p>
            )}
          </div>
        </div>

        <div className="tcard__side">
          {copied ? (
            <span className="tcard__copied">
              <Check size={14} strokeWidth={2.5} /> Copied
            </span>
          ) : fixing ? (
            <IconButton
              label="Cancel"
              onClick={(e) => {
                stop(e);
                cancelFix();
              }}
            >
              <X size={16} />
            </IconButton>
          ) : (
            <div className="tcard__actions">
              {hasAudio && (
                <IconButton
                  label={audioOpen && !audioPaused ? "Pause recording" : "Play recording"}
                  className={audioOpen ? "is-active" : ""}
                  onClick={togglePlay}
                >
                  {audioOpen && !audioPaused ? <Pause size={15} strokeWidth={2} /> : <Play size={15} strokeWidth={2} />}
                </IconButton>
              )}
              <IconButton
                label="Copy dictation"
                onClick={(e) => {
                  stop(e);
                  void copy();
                }}
              >
                <Copy size={15} strokeWidth={1.9} />
              </IconButton>
              <IconButton label="Fix a word" onClick={startFix}>
                <PenLine size={15} strokeWidth={1.9} />
              </IconButton>
              {confirmDelete ? (
                <button type="button" className="tcard__confirm" onClick={del}>
                  Delete?
                </button>
              ) : (
                <IconButton label="Delete" tone="danger" onClick={del}>
                  <Trash2 size={15} strokeWidth={1.9} />
                </IconButton>
              )}
            </div>
          )}
        </div>
      </div>

      {audioOpen && (
        <div className="audio-player" onClick={stop}>
          <Mic size={14} strokeWidth={2} aria-hidden="true" />
          <input
            type="range"
            aria-label="Recording position"
            min={0}
            max={audioDuration || 1}
            step={0.05}
            value={Math.min(audioTime, audioDuration || 1)}
            onChange={(e) => {
              const t = Number(e.target.value);
              setAudioTime(t);
              if (audioRef.current) audioRef.current.currentTime = t;
            }}
          />
          <span className="audio-player__time num">
            {formatAudioClock(audioTime)} / {formatAudioClock(audioDuration)}
          </span>
        </div>
      )}

      {fixing && (
        <div className="tcard__fix" onClick={stop}>
          <p className="tcard__text tcard__text--words">
            {tokens.map((t, i) =>
              t.word ? (
                <span key={i}>
                  {t.lead}
                  <button
                    type="button"
                    className={`word ${picked === i ? "is-picked" : ""}`}
                    onClick={() => {
                      setPicked(i);
                      setReplacement(t.core);
                    }}
                  >
                    {t.core}
                  </button>
                  {t.trail}
                </span>
              ) : (
                <span key={i}>{t.text}</span>
              ),
            )}
          </p>
          <div className="fixbar">
            {picked === null ? (
              <span className="fixbar__prompt">Click the word Wren got wrong.</span>
            ) : (
              <>
                <span className="fixbar__from">{tokens[picked].core}</span>
                <span className="fixbar__arrow" aria-hidden="true">
                  →
                </span>
                <input
                  ref={inputRef}
                  className="input fixbar__input"
                  value={replacement}
                  onChange={(e) => setReplacement(e.target.value)}
                  onKeyDown={onInputKey}
                  onFocus={(e) => e.currentTarget.select()}
                  aria-label={`Replace ${tokens[picked].core} with`}
                  spellCheck={false}
                />
                <Button
                  variant="primary"
                  size="sm"
                  disabled={saving || !replacement.trim() || replacement.trim() === tokens[picked].core}
                  onClick={() => void saveFix()}
                >
                  Save
                </Button>
              </>
            )}
            {picked !== null && <span className="fixbar__note">Wren will spell it this way from now on.</span>}
          </div>
        </div>
      )}
    </article>
  );
}
