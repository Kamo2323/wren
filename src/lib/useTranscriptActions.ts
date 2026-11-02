// The copy / delete / fix-a-word handlers every list of transcript cards shares (Home and History).
import { useCallback, useMemo, type Dispatch, type SetStateAction } from "react";
import { createElement } from "react";
import type { TranscriptionRow } from "./types";
import { copyText, historyDelete, historyFixWord, replaceWord } from "./api";
import { dayLabel } from "./format";
import { useToast } from "../components/Toast";
import { Check } from "../components/Icon";

type SetRows = Dispatch<SetStateAction<TranscriptionRow[] | null>>;

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
        await historyFixWord(row.id, from, to);
        setRows((rs) => (rs ?? []).map((r) => (r.id === row.id ? { ...r, finalText: replaceWord(r.finalText, from, to) } : r)));
        toast.show(`Fixed. Wren will write “${to}” from now on.`, {
          tone: "success",
          icon: createElement(Check, { size: 16, strokeWidth: 2.6 }),
          ms: 3200,
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
