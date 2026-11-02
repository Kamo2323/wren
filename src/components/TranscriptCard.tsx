// One dictation as a big card. Click anywhere on it to copy its text (the way back when a paste goes
// wrong). Hover shows Fix a word and Delete. Fix turns the text into clickable words: pick one, type
// what it should be, Save.
import { useEffect, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import type { TranscriptionRow } from "../lib/types";
import { appName, whenLabel } from "../lib/format";
import { markMatches } from "../lib/text";
import { Check, Copy, PenLine, Trash2, X } from "./Icon";
import { Button, IconButton } from "./Button";

export interface TranscriptCardProps {
  row: TranscriptionRow;
  onCopy: (row: TranscriptionRow) => Promise<boolean>;
  /** Rejects if the delete failed; the card then comes back. */
  onDelete: (row: TranscriptionRow) => Promise<void>;
  onFix: (row: TranscriptionRow, from: string, to: string) => Promise<void>;
  now?: number;
  /** Search words to mark in the text (History search). The whole text shows while searching. */
  highlight?: string;
}


type Token = { text: string; word: boolean; core: string; lead: string; trail: string };

/** Split text into words and the spaces between them; a word keeps its punctuation apart from its core. */
export function tokenise(text: string): Token[] {
  return text.split(/(\s+)/).filter(Boolean).map((part) => {
    if (/^\s+$/.test(part)) return { text: part, word: false, core: "", lead: "", trail: "" };
    const m = part.match(/^([^\p{L}\p{N}]*)(.*?)([^\p{L}\p{N}]*)$/u);
    const [lead, core, trail] = m ? [m[1], m[2], m[3]] : ["", part, ""];
    return { text: part, word: core.length > 0, core, lead, trail };
  });
}

const COPIED_MS = 1600;

export function TranscriptCard({ row, onCopy, onDelete, onFix, now, highlight }: TranscriptCardProps) {
  const [copied, setCopied] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [fixing, setFixing] = useState(false);
  const [picked, setPicked] = useState<number | null>(null);
  const [replacement, setReplacement] = useState("");
  const [saving, setSaving] = useState(false);
  const [leaving, setLeaving] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const timers = useRef<number[]>([]);

  useEffect(() => () => timers.current.forEach((t) => window.clearTimeout(t)), []);
  useEffect(() => {
    if (picked !== null) inputRef.current?.focus();
  }, [picked]);

  const tokens = fixing ? tokenise(row.finalText) : [];
  const handsFree = row.mode === "handsFree" || row.mode === "handsfree";
  const notPasted = row.pasteOutcome === "notPasted" || row.pasteOutcome === "not_pasted" || row.pasteOutcome === "failed";

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
    setLeaving(true);
    timers.current.push(
      window.setTimeout(() => {
        onDelete(row).catch(() => {
          // the page has said it failed; bring the card back
          setLeaving(false);
          setConfirmDelete(false);
        });
      }, 200),
    );
  }

  function onKey(e: KeyboardEvent<HTMLDivElement>) {
    // keys on the buttons inside the card (Fix a word, Delete) are theirs, not a copy
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

  const cls = ["tcard", copied && "is-copied", fixing && "is-fixing", leaving && "is-leaving"].filter(Boolean).join(" ");

  return (
    <div
      className={cls}
      role={fixing ? undefined : "button"}
      tabIndex={fixing ? -1 : 0}
      aria-label={fixing ? undefined : `Copy: ${row.finalText.slice(0, 80)}`}
      onClick={copy}
      onKeyDown={onKey}
    >
      <div className="tcard__meta">
        <span className="tcard__app">{appName(row.targetProcess) || "Wren"}</span>
        <span className="tcard__dot" aria-hidden="true" />
        <span>{whenLabel(row.createdAtUtc, now)}</span>
        <span className="tcard__dot" aria-hidden="true" />
        <span className="num">{row.words} {row.words === 1 ? "word" : "words"}</span>
        {handsFree && <span className="chip chip--quiet">Hands-free</span>}
        {notPasted && <span className="chip chip--warn">Not pasted</span>}

        <div className="tcard__side">
          {copied ? (
            <span className="tcard__copied">
              <Check size={15} strokeWidth={2.6} /> Copied
            </span>
          ) : fixing ? (
            <IconButton label="Cancel" onClick={(e) => { stop(e); cancelFix(); }}>
              <X size={17} />
            </IconButton>
          ) : (
            <>
              <span className="tcard__hint">
                <Copy size={14} strokeWidth={2} /> Click to copy
              </span>
              <div className="tcard__actions">
                <IconButton label="Fix a word" onClick={startFix}>
                  <PenLine size={17} strokeWidth={1.9} />
                </IconButton>
                {confirmDelete ? (
                  <button type="button" className="tcard__confirm" onClick={del}>
                    Delete?
                  </button>
                ) : (
                  <IconButton label="Delete" tone="danger" onClick={del}>
                    <Trash2 size={17} strokeWidth={1.9} />
                  </IconButton>
                )}
              </div>
            </>
          )}
        </div>
      </div>

      {fixing ? (
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
                <span className="fixbar__arrow" aria-hidden="true">→</span>
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
            <span className="fixbar__note">Wren will spell it this way from now on.</span>
          </div>
        </div>
      ) : (
        <p className={`tcard__text ${highlight?.trim() ? "tcard__text--full" : ""}`}>
          {highlight?.trim()
            ? markMatches(row.finalText, highlight).map((p, i) => (p.match ? <mark key={i}>{p.text}</mark> : <span key={i}>{p.text}</span>))
            : row.finalText}
        </p>
      )}
    </div>
  );
}
