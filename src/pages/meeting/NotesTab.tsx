// My notes: a simple Notion-like editor (headings, bullets, checkboxes, bold), autosaved as TipTap JSON.
import { useEffect, useRef, useState } from "react";
import { EditorContent, useEditor, useEditorState } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import TaskList from "@tiptap/extension-task-list";
import TaskItem from "@tiptap/extension-task-item";
import { meetingUpdate } from "../../lib/meetings";
import { Bold, Heading2, List, ListChecks } from "../../components/Icon";
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

export function NotesTab({ meetingId, initial }: { meetingId: string; initial: string }) {
  const pending = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [saved, setSaved] = useState<"idle" | "saving" | "saved" | "failed">("idle");

  const flush = () => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    const json = pending.current;
    pending.current = null;
    if (json === null) return;
    setSaved("saving");
    void meetingUpdate(meetingId, { myNotes: json })
      .then(() => setSaved("saved"))
      .catch(() => setSaved("failed"));
  };

  const editor = useEditor({
    extensions: [StarterKit.configure({ link: false }), TaskList, TaskItem.configure({ nested: true })],
    content: parse(initial),
    editorProps: { attributes: { class: "notes-editor", "aria-label": "My notes", spellcheck: "true" } },
    onUpdate: ({ editor }) => {
      pending.current = editor.isEmpty ? "" : JSON.stringify(editor.getJSON());
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(flush, SAVE_DEBOUNCE_MS);
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

  // Save on leaving the tab or the page.
  useEffect(() => () => flush(), []); // eslint-disable-line react-hooks/exhaustive-deps

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
