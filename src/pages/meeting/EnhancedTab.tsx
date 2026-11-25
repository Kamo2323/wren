// Enhanced notes: Short / Medium / Long written by Claude. Streams while writing; a note is only kept once
// it finished. Errors say what to do; "Notes changed" offers a rewrite after edits (spec §5.4, §6.3).
import { useMemo, useState, type MouseEvent } from "react";
import { Marked } from "marked";
import DOMPurify from "dompurify";
import type { Length, NoteErrorKind, NoteRow } from "../../lib/meetings";
import { footerLabel } from "../../lib/meetingText";
import { copyText, inTauri } from "../../lib/api";
import { Button } from "../../components/Button";
import { useToast } from "../../components/Toast";
import { Copy, KeyRound, RotateCw, Sparkles } from "../../components/Icon";

export interface NoteError {
  kind: NoteErrorKind;
  message: string;
}

const LENGTHS: Array<{ id: Length; label: string }> = [
  { id: "short", label: "Short" },
  { id: "medium", label: "Medium" },
  { id: "long", label: "Long" },
];

/** Markdown parser for notes: raw HTML in the model's output is dropped, never rendered. */
const notesMarked = new Marked({ gfm: true, breaks: false, renderer: { html: () => "" } });

/** Only the tags note formatting needs; nothing that can load a resource. */
const ALLOWED_TAGS = [
  "p", "h1", "h2", "h3", "h4", "ul", "ol", "li", "strong", "em", "del", "code", "pre", "blockquote",
  "table", "thead", "tbody", "tr", "th", "td", "a", "hr", "br", "input",
];
const ALLOWED_ATTR = ["href", "type", "checked", "disabled", "align"];

/** Markdown → sanitised HTML. */
function toHtml(md: string): string {
  const html = notesMarked.parse(md, { async: false }) as string;
  return DOMPurify.sanitize(html, { ALLOWED_TAGS, ALLOWED_ATTR, ALLOW_DATA_ATTR: false });
}

/** Links never navigate Wren's own window: web links open in the browser, anything else is ignored. */
function onLinkClick(e: MouseEvent<HTMLDivElement>) {
  const a = (e.target as HTMLElement).closest("a");
  if (!a) return;
  e.preventDefault();
  const href = a.getAttribute("href") ?? "";
  if (!/^https?:\/\//i.test(href)) return;
  if (inTauri) void import("@tauri-apps/plugin-opener").then((m) => m.openUrl(href)).catch(() => {});
  // Browser preview (no Tauri): just note it, so the page checks can see where it would have gone.
  else (window as unknown as { __wrenOpenedLink?: string }).__wrenOpenedLink = href;
}

function Markdown({ md }: { md: string }) {
  const html = useMemo(() => toHtml(md), [md]);
  return <div className="md" onClick={onLinkClick} dangerouslySetInnerHTML={{ __html: html }} />;
}

/** Copy as Markdown text through Wren's native clipboard path (pages never use the web clipboard). */
function copyNotes(md: string): Promise<boolean> {
  return copyText(md);
}

function errorAdvice(e: NoteError): string {
  switch (e.kind) {
    case "no_key":
      return "Add your Claude API key in Settings to write notes.";
    case "unauthorized":
      return "Claude rejected your API key. Check it in Settings.";
    case "refusal":
      return "Claude declined to summarise this meeting.";
    case "rate_limited":
    case "overloaded":
      return "Claude is busy right now. Try again in a minute.";
    case "network":
      return "Couldn't reach Claude. Check your internet connection and try again.";
    default:
      return e.message || "Something went wrong writing the notes.";
  }
}

export function EnhancedTab({
  notes,
  streams,
  writing,
  errors,
  live,
  hasKey,
  onGenerate,
  onOpenSettings,
}: {
  notes: NoteRow[];
  streams: Partial<Record<Length, string>>;
  writing: Length[];
  errors: Partial<Record<Length, NoteError>>;
  live: boolean;
  hasKey: boolean;
  onGenerate: (len: Length, regenerate: boolean) => void;
  onOpenSettings: () => void;
}) {
  const toast = useToast();
  const [len, setLen] = useState<Length>(() => (writing[0] ?? "medium"));
  const note = notes.find((n) => n.length === len);
  const stream = streams[len];
  const isWriting = writing.includes(len);
  const error = errors[len];
  const keyProblem = error?.kind === "no_key" || error?.kind === "unauthorized";

  function pick(l: Length) {
    setLen(l);
    // Spec §5.4: Short and Long are written the first time they're picked.
    const exists = notes.some((n) => n.length === l);
    if (!exists && !writing.includes(l) && !errors[l] && hasKey && !live) onGenerate(l, false);
  }

  let body;
  if (isWriting) {
    body = (
      <>
        {stream ? <Markdown md={stream} /> : <p className="enh-wait">Reading the transcript…</p>}
        <p className="enh-writing" role="status">
          <span className="spinner" aria-hidden="true" /> Writing {len} notes…
        </p>
      </>
    );
  } else if (error) {
    body = (
      <div className="enh-error" role="alert">
        <p>{errorAdvice(error)}</p>
        <div className="enh-actions">
          {keyProblem ? (
            <Button variant="primary" icon={<KeyRound size={14} strokeWidth={2.2} />} onClick={onOpenSettings}>
              Open Settings
            </Button>
          ) : (
            <Button variant="primary" icon={<RotateCw size={14} strokeWidth={2.2} />} onClick={() => onGenerate(len, true)}>
              Retry
            </Button>
          )}
        </div>
        {note && <Markdown md={note.markdown} />}
      </div>
    );
  } else if (note) {
    body = (
      <>
        {note.stale && (
          <div className="enh-stale">
            <span>Notes changed since these were written.</span>
            <Button size="sm" variant="secondary" icon={<RotateCw size={13} strokeWidth={2.2} />} onClick={() => onGenerate(len, true)}>
              Regenerate
            </Button>
          </div>
        )}
        <Markdown md={note.markdown} />
        <div className="enh-bar">
          <span className="enh-footer num">{footerLabel(note)}</span>
          <div className="enh-actions">
            <Button
              size="sm"
              variant="ghost"
              icon={<Copy size={13} strokeWidth={2.2} />}
              onClick={() => void copyNotes(note.markdown).then((ok) => toast.show(ok ? "Notes copied" : "Couldn't copy the notes"))}
            >
              Copy
            </Button>
            <Button size="sm" variant="ghost" icon={<RotateCw size={13} strokeWidth={2.2} />} onClick={() => onGenerate(len, true)}>
              Regenerate
            </Button>
          </div>
        </div>
      </>
    );
  } else if (live) {
    body = <p className="enh-empty">Notes are written when the meeting ends. Press Stop when you're done.</p>;
  } else if (!hasKey) {
    body = (
      <div className="enh-empty">
        <p>Add your Claude API key in Settings to write notes from this meeting.</p>
        <Button variant="primary" icon={<KeyRound size={14} strokeWidth={2.2} />} onClick={onOpenSettings}>
          Open Settings
        </Button>
      </div>
    );
  } else {
    body = (
      <div className="enh-empty">
        <p>No {len} notes yet.</p>
        <Button variant="primary" icon={<Sparkles size={14} strokeWidth={2.2} />} onClick={() => onGenerate(len, false)}>
          Write notes
        </Button>
      </div>
    );
  }

  return (
    <div className="enhanced">
      <div className="segments enh-lengths" role="group" aria-label="Length">
        {LENGTHS.map((l) => (
          <button key={l.id} type="button" className="segments__btn" aria-pressed={len === l.id} onClick={() => pick(l.id)}>
            {l.label}
            {writing.includes(l.id) && <span className="seg-dot" aria-hidden="true" />}
          </button>
        ))}
      </div>
      <div className="enh-body">{body}</div>
    </div>
  );
}
