// The copy / delete / fix-a-word handlers every list of transcript cards shares (Home and History).
import { useCallback, useMemo, useRef, type Dispatch, type SetStateAction } from "react";
import { createElement } from "react";
import type { TranscriptionRow } from "./types";
import { copyText, dictionarySetStatus, historyDelete, historyFixWord } from "./api";
import { replaceWord, wordCount } from "./text";
import { dayLabel } from "./format";
import { useToast } from "../components/Toast";
import { Check } from "../components/Icon";

type SetRows = Dispatch<SetStateAction<TranscriptionRow[] | null>>;

/** The newest dictation time (ms) each page has shown, kept across visits (module scope = this app session). */
const newestSeen: Record<string, number> = {};

/**
 * Ids of rows newer than anything `page` has shown before (none on the very first load), so only
 * genuinely new cards animate in; revisiting a page shows its cards at once.
 */
export function useNewIds(rows: TranscriptionRow[] | null, page: string): ReadonlySet<string> {
  const memo = useRef<{ rows: TranscriptionRow[] | null; ids: ReadonlySet<string> }>({ rows: null, ids: new Set() });
  if (rows !== memo.current.rows) {
    const before = newestSeen[page];
    const ids = new Set<string>();
    if (rows) {
      let newest = before ?? -Infinity;
      for (const r of rows) {
        const t = Date.parse(r.createdAtUtc);
        if (before != null && t > before) ids.add(r.id);
        if (t > newest) newest = t;
      }
      newestSeen[page] = newest; // an empty first load leaves -Infinity: the first dictation is new
    }
    memo.current = { rows, ids };
  }
  return memo.current.ids;
}

export function useTranscriptActions(setRows: SetRows) {
  const toast = useToast();

  const onCopy = useCallback(
    async (row: TranscriptionRow) => {
      const ok = await copyText(row.finalText);
      if (!ok) toast.show("Couldn't copy. Try again.", { tone: "danger" });
      return ok;
    },
    [toast],
  );

  const onDelete = useCallback(
    async (row: TranscriptionRow) => {
      try {
        await historyDelete(row.id);
        setRows((rs) => (rs ?? []).filter((r) => r.id !== row.id));
        toast.show("Deleted");
      } catch (err) {
        toast.show("Couldn't delete that one.", { tone: "danger" });
        throw err;
      }
    },
    [toast, setRows],
  );

  const onFix = useCallback(
    async (row: TranscriptionRow, from: string, to: string) => {
      try {
        const result = await historyFixWord(row.id, from, to);
        const before = { finalText: row.finalText, words: row.words };
        const fixed = replaceWord(row.finalText, from, to); // the same rewrite the store made
        setRows((rs) => (rs ?? []).map((r) => (r.id === row.id ? { ...r, finalText: fixed, words: wordCount(fixed) } : r)));
        const undo = async () => {
          try {
            // Stop the rule (and the word, if this fix added it); disabled = "the user said no" (spec §8).
            await dictionarySetStatus(result.replacement.id, "disabled");
            if (result.term) await dictionarySetStatus(result.term.id, "disabled");
            // The card shows its old text again. The store keeps the fixed text (there is no command to
            // put text back yet), so a reload shows the fix: see task-7-report fix round 1, finding 11.
            setRows((rs) => (rs ?? []).map((r) => (r.id === row.id ? { ...r, ...before } : r)));
            toast.show(`Undone. Wren won't change “${from}” any more.`);
          } catch {
            toast.show("Couldn't undo that fix.", { tone: "danger" });
          }
        };
        toast.show(`Fixed. Wren will write “${to}” from now on.`, {
          tone: "success",
          icon: createElement(Check, { size: 16, strokeWidth: 2.6 }),
          action: { label: "Undo", onClick: () => void undo() },
        });
      } catch {
        toast.show("Couldn't save that fix.", { tone: "danger" });
        throw new Error("fix failed");
      }
    },
    [toast, setRows],
  );

  return { onCopy, onDelete, onFix };
}

/** Rows (newest first) grouped under "Today", "Yesterday", "Sat 31 Oct"… */
export function useDayGroups(rows: TranscriptionRow[] | null, now: number) {
  return useMemo(() => {
    const out: Array<{ label: string; rows: TranscriptionRow[] }> = [];
    for (const r of rows ?? []) {
      const label = dayLabel(r.createdAtUtc, new Date(now));
      const g = out[out.length - 1];
      if (g && g.label === label) g.rows.push(r);
      else out.push({ label, rows: [r] });
    }
    return out;
  }, [rows, now]);
}
