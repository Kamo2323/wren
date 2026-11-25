// My notes: a simple Notion-like editor (headings, bullets, checkboxes, bold), autosaved as TipTap JSON.
import { useEffect, useRef, useState, type MutableRefObject } from "react";
import { EditorContent, useEditor, useEditorState } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import { Bold, Heading2, List, ListChecks } from "../../components/Icon";
import { getNotesDraft, setNotesDraft } from "../../lib/meetings";
import type { ReactNode } from "react";

const SAVE_DEBOUNCE_MS = 800;

function parse(doc: string): object | string {
  if (!doc) return "";
  try {
    return JSON.parse(doc) as object;
  } catch {
    return `<p>${doc.replace(/&/g, "&amp;").replace(/</g, "&lt;")}</p>`;
  }
}

export function NotesTab({
  meetingId,
  initial,
  save,
  flushRef,
}: {
  meetingId: string;
  /** The latest notes (the meeting view keeps it current). */
  initial: string;
  /** Save the latest draft on the meeting's ordered save queue. */
  save: () => Promise<void>;
  /** The meeting view calls this before Stop so the automatic notes include the last edit. */
  flushRef: MutableRefObject<(() => Promise<void>) | null>;
}) {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const alive = useRef(true);
  const revision = useRef(0);
  const [saved, setSaved] = useState<"idle" | "saving" | "saved" | "failed">("idle");

  const flush = async () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    if (getNotesDraft(meetingId) === undefined) return;
    const writing = revision.current;
    if (alive.current) setSaved("saving");
    try {
      await save();
      if (alive.current && revision.current === writing) setSaved("saved");
    } catch (error) {
      if (alive.current && revision.current === writing) setSaved("failed");
      throw error;
    }
  };

  const editor = useEditor({
    extensions: [StarterKit.configure({ link: false }), TaskList, TaskItem.configure({ nested: true })],
    content: parse(getNotesDraft(meetingId) ?? initial),
    editorProps: { attributes: { class: "notes-editor", "aria-label": "My notes", spellcheck: "true" } },
    onUpdate: ({ editor }) => {
      revision.current++;
      setNotesDraft(meetingId, editor.isEmpty ? "" : JSON.stringify(editor.getJSON()));
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => void flush().catch(() => {}), SAVE_DEBOUNCE_MS);
    },
  });
  const state = useEditorState({
    editor,
    selector: ({ editor: e }) => ({
      empty: e?.isEmpty ?? true,
      h: e?.isActive("heading", { level: 2 }) ?? false,
      bullet: e?.isActive("bulletList") ?? false,
      task: e?.isActive("taskList") ?? false,
      bold: e?.isActive("bold") ?? false,
    }),
  });

  // Save on leaving the tab or the page; let the parent flush before Stop.
  useEffect(() => {
    alive.current = true;
    flushRef.current = flush;
    return () => {
      alive.current = false;
      void flush().catch(() => {}); // the draft stays available to retry after the editor closes
      flushRef.current = null;
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps

  const tool = (label: string, active: boolean | undefined, run: () => void, text: ReactNode) => (
    <button
      type="button"
      className={`notes-tool ${active ? "is-active" : ""}`}
      aria-pressed={!!active}
      aria-label={label}
      title={label}
      onMouseDown={(e) => e.preventDefault()}
      onClick={run}
    >
      {text}
    </button>
  );

  return (
    <div className="notes">
      <div className="notes-toolbar" role="toolbar" aria-label="Formatting">
        {tool("Heading", state?.h, () => editor?.chain().focus().toggleHeading({ level: 2 }).run(), <Heading2 size={16} strokeWidth={2} />)}
        {tool("Bullet list", state?.bullet, () => editor?.chain().focus().toggleBulletList().run(), <List size={16} strokeWidth={2} />)}
        {tool("Checklist", state?.task, () => editor?.chain().focus().toggleTaskList().run(), <ListChecks size={16} strokeWidth={2} />)}
        {tool("Bold", state?.bold, () => editor?.chain().focus().toggleBold().run(), <Bold size={15} strokeWidth={2.2} />)}
        <span className={`notes-saved notes-saved--${saved}`} aria-live="polite">
          {saved === "saving" ? "Saving…" : saved === "saved" ? "Saved" : saved === "failed" ? "Couldn't save" : ""}
        </span>
      </div>
      <div className="notes-body">
        {state?.empty && (
          <p className="notes-hint" aria-hidden="true">
            Jot down what matters to you. Claude uses your notes to decide what to focus on.
          </p>
        )}
        <EditorContent editor={editor} />
      </div>
    </div>
  );
}
