// Dictionary: Words (teal), Replacements (violet) and Learned (amber) with semantic tinted lists,
// inline add forms, toggle switches, candidate promotion, Undo and Delete.
import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import type { DictionaryEntry, EntrySource, EntryStatus } from "../lib/types";
import { dictionaryAddReplacement, dictionaryAddTerm, dictionaryDelete, dictionaryList, dictionarySetStatus } from "../lib/api";
import { formatNumber } from "../lib/format";
import { Tabs } from "../components/Tabs";
import { Button, IconButton } from "../components/Button";
import { Toggle } from "../components/Toggle";
import { EmptyState, LoadError } from "../components/Empty";
import { useToast } from "../components/Toast";
import { ArrowLeftRight, ArrowRight, BookA, Check, Plus, RotateCcw, Sparkles, Trash2, Undo2 } from "../components/Icon";

type TabId = "words" | "replacements" | "learned";
type StatusChange = { id: number; status: EntryStatus };

const LEARNED: EntrySource[] = ["learnedHistory", "learnedPaste"];
const isLearned = (e: DictionaryEntry) => LEARNED.includes(e.source);
const lower = (s: string) => s.trim().toLocaleLowerCase("en-GB");
const MAX_LEN = 80;

const TAB_KEY = "wren.dictionary.tab";
function savedTab(): TabId {
  try {
    const t = sessionStorage.getItem(TAB_KEY);
    if (t === "words" || t === "replacements" || t === "learned") return t;
  } catch {
    /* storage blocked */
  }
  return "words";
}

export function usedLabel(hits: number): string {
  if (hits === 0) return "Not used yet";
  if (hits === 1) return "Used once";
  return `Used ${formatNumber(hits)} times`;
}

interface LearnedRow {
  main: DictionaryEntry;
  term: DictionaryEntry | null;
}

function learnedRows(entries: DictionaryEntry[]): LearnedRow[] {
  const learned = entries.filter(isLearned).sort((a, b) => b.createdAt.localeCompare(a.createdAt) || b.id - a.id);
  const reps = learned.filter((e) => e.kind === "replacement");
  const terms = learned.filter((e) => e.kind === "term");
  const used = new Set<number>();
  const rows: LearnedRow[] = reps.map((r) => {
    const t = terms.find((t) => !used.has(t.id) && lower(t.toText) === lower(r.toText)) ?? null;
    if (t) used.add(t.id);
    return { main: r, term: t };
  });
  for (const t of terms) if (!used.has(t.id)) rows.push({ main: t, term: null });
  return rows.sort((a, b) => b.main.createdAt.localeCompare(a.main.createdAt) || b.main.id - a.main.id);
}

// ---------------------------------------------------------------- add forms

function AddWord({ existing, onAdd }: { existing: DictionaryEntry[]; onAdd: (word: string) => Promise<void> }) {
  const [word, setWord] = useState("");
  const [busy, setBusy] = useState(false);
  const w = word.trim();
  const match = w === "" ? undefined : existing.find((e) => e.kind === "term" && lower(e.toText) === lower(w));
  const dupe = match !== undefined;
  const dupeText = !match ? "" : isLearned(match) ? "Wren has already learned that one. It's under Learned." : "That one is already in your words.";

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!w || dupe || busy) return;
    setBusy(true);
    try {
      await onAdd(w);
      setWord("");
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="addbar" onSubmit={submit}>
      <div className="addbar__fields">
        <label className="field field--grow">
          <span className="field__label">Word or name</span>
          <input
            className="input input--lg"
            value={word}
            maxLength={MAX_LEN}
            placeholder="e.g. Siobhan, Kubernetes, WebView2"
            spellCheck={false}
            onChange={(e) => setWord(e.target.value)}
            aria-invalid={dupe}
            aria-describedby="add-word-hint"
          />
        </label>
        <Button type="submit" variant="primary" icon={<Plus size={16} strokeWidth={2.2} />} disabled={!w || dupe || busy}>
          Add word
        </Button>
      </div>
      <p id="add-word-hint" className={`addbar__hint ${dupe ? "is-warn" : ""}`}>
        {dupe ? dupeText : "Give Wren the spellings of names and terms you use often."}
      </p>
    </form>
  );
}

function AddReplacement({ existing, onAdd }: { existing: DictionaryEntry[]; onAdd: (from: string, to: string) => Promise<void> }) {
  const [from, setFrom] = useState("");
  const [to, setTo] = useState("");
  const [busy, setBusy] = useState(false);
  const fromRef = useRef<HTMLInputElement>(null);
  const f = from.trim();
  const t = to.trim();
  const same = f !== "" && f === t;
  const match =
    f === "" ? undefined : existing.find((e) => e.kind === "replacement" && e.status !== "disabled" && e.fromText != null && lower(e.fromText) === lower(f));
  const dupe = match !== undefined;
  const hint = same
    ? "Those are the same. Type what Wren should write instead."
    : match
      ? isLearned(match)
        ? `Wren has already learned “${match.fromText}” → “${match.toText}”. It's under Learned.`
        : `There's already a replacement for “${match.fromText}”, just below.`
      : "Whole words only, and it keeps your capitals at the start of a sentence.";

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!f || !t || same || dupe || busy) return;
    setBusy(true);
    try {
      await onAdd(f, t);
      setFrom("");
      setTo("");
      fromRef.current?.focus();
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="addbar" onSubmit={submit}>
      <div className="addbar__fields">
        <label className="field field--grow">
          <span className="field__label">When Wren hears</span>
          <input
            ref={fromRef}
            className="input input--lg"
            value={from}
            maxLength={MAX_LEN}
            placeholder="e.g. pie test"
            spellCheck={false}
            onChange={(e) => setFrom(e.target.value)}
            aria-invalid={dupe || same}
            aria-describedby="add-rep-hint"
          />
        </label>
        <ArrowRight className="addbar__arrow" size={18} strokeWidth={2} aria-hidden="true" />
        <label className="field field--grow">
          <span className="field__label">Write</span>
          <input
            className="input input--lg"
            value={to}
            maxLength={MAX_LEN}
            placeholder="e.g. pytest"
            spellCheck={false}
            onChange={(e) => setTo(e.target.value)}
            aria-invalid={same}
            aria-describedby="add-rep-hint"
          />
        </label>
        <Button type="submit" variant="primary" icon={<Plus size={16} strokeWidth={2.2} />} disabled={!f || !t || same || dupe || busy}>
          Add
        </Button>
      </div>
      <p id="add-rep-hint" className={`addbar__hint ${same || dupe ? "is-warn" : ""}`}>
        {hint}
      </p>
    </form>
  );
}

// ---------------------------------------------------------------- rows

function DeleteButton({ onDelete, label }: { onDelete: () => void; label: string }) {
  const [confirm, setConfirm] = useState(false);
  useEffect(() => {
    if (!confirm) return;
    const t = window.setTimeout(() => setConfirm(false), 3500);
    return () => window.clearTimeout(t);
  }, [confirm]);
  return confirm ? (
    <button type="button" className="tcard__confirm" onClick={onDelete}>
      Delete?
    </button>
  ) : (
    <IconButton label={label} tone="danger" onClick={() => setConfirm(true)}>
      <Trash2 size={15} strokeWidth={1.9} />
    </IconButton>
  );
}

function Swap({ from, to }: { from: string; to: string }) {
  return (
    <span className="swap">
      <span className="swap__from">{from}</span>
      <ArrowRight className="swap__arrow" size={15} strokeWidth={2} aria-hidden="true" />
      <span className="swap__to">{to}</span>
    </span>
  );
}

function Row({ icon, children, muted, className = "" }: { icon: ReactNode; children: ReactNode; muted?: boolean; className?: string }) {
  return (
    <li className={`drow ${muted ? "is-muted" : ""} ${className}`}>
      <span className="tinted-icon" aria-hidden="true">
        {icon}
      </span>
      {children}
    </li>
  );
}

// ---------------------------------------------------------------- page

export default function Dictionary() {
  const [entries, setEntries] = useState<DictionaryEntry[] | null>(null);
  const [tab, setTabState] = useState<TabId>(savedTab);
  const toast = useToast();

  const setTab = (t: TabId) => {
    setTabState(t);
    try {
      sessionStorage.setItem(TAB_KEY, t);
    } catch {
      /* storage blocked */
    }
  };

  const [failed, setFailed] = useState(false);
  const load = useCallback(async () => {
    try {
      setEntries(await dictionaryList());
      setFailed(false);
    } catch {
      setFailed(true);
    }
  }, []);

  useEffect(() => {
    void load();
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [load]);

  const all = entries ?? [];
  const words = useMemo(
    () => all.filter((e) => e.kind === "term" && !isLearned(e)).sort((a, b) => a.toText.localeCompare(b.toText, "en-GB", { sensitivity: "base" })),
    [all],
  );
  const replacements = useMemo(
    () => all.filter((e) => e.kind === "replacement" && !isLearned(e)).sort((a, b) => (a.fromText ?? "").localeCompare(b.fromText ?? "", "en-GB", { sensitivity: "base" })),
    [all],
  );
  const learned = useMemo(() => learnedRows(all), [all]);

  const entriesRef = useRef(entries);
  entriesRef.current = entries;
  const statusOf = (e: DictionaryEntry) => entriesRef.current?.find((x) => x.id === e.id)?.status ?? e.status;

  const patch = (id: number, change: Partial<DictionaryEntry>) =>
    setEntries((es) => (es ?? []).map((e) => (e.id === id ? { ...e, ...change } : e)));
  const drop = (ids: number[]) => setEntries((es) => (es ?? []).filter((e) => !ids.includes(e.id)));
  const fail = (what: string) => toast.show(`Couldn't ${what}. Try again.`, { tone: "danger" });

  async function addWord(word: string) {
    try {
      const e = await dictionaryAddTerm(word);
      setEntries((es) => [...(es ?? []).filter((x) => x.id !== e.id), e]);
      toast.show(`Added “${e.toText}”`, { tone: "success", icon: <Check size={15} strokeWidth={2.5} /> });
    } catch {
      fail("add that word");
      throw new Error("add failed");
    }
  }

  async function removeWord(e: DictionaryEntry) {
    try {
      await dictionaryDelete(e.id);
      drop([e.id]);
      toast.show(`Removed “${e.toText}”`, {
        action: { label: "Undo", onClick: () => void addWord(e.toText).catch(() => {}) },
      });
    } catch {
      fail("remove that word");
    }
  }

  async function addReplacement(from: string, to: string) {
    try {
      const e = await dictionaryAddReplacement(from, to);
      setEntries((es) => [...(es ?? []).filter((x) => x.id !== e.id), e]);
      toast.show(`Wren will write “${to}” for “${from}”`, { tone: "success", icon: <Check size={15} strokeWidth={2.5} /> });
      void load();
    } catch {
      fail("add that replacement");
      throw new Error("add failed");
    }
  }

  async function setOn(e: DictionaryEntry, on: boolean) {
    const ok = await applyStatuses([{ id: e.id, status: on ? "active" : "disabled" }], [{ id: e.id, status: statusOf(e) }]);
    if (!ok) fail(on ? "turn that on" : "turn that off");
  }

  async function applyStatuses(changes: StatusChange[], previous: StatusChange[]): Promise<boolean> {
    changes.forEach((c) => patch(c.id, { status: c.status }));
    try {
      await Promise.all(changes.map((c) => dictionarySetStatus(c.id, c.status)));
      if (changes.some((c) => c.status === "active")) void load();
      return true;
    } catch {
      previous.forEach((p) => patch(p.id, { status: p.status }));
      void load();
      return false;
    }
  }

  async function removeReplacement(e: DictionaryEntry) {
    try {
      await dictionaryDelete(e.id);
      drop([e.id]);
      toast.show(`Removed “${e.fromText}” → “${e.toText}”`, {
        action: { label: "Undo", onClick: () => void addReplacement(e.fromText ?? "", e.toText).catch(() => {}) },
      });
    } catch {
      fail("remove that replacement");
    }
  }

  function termGoesWith(row: LearnedRow): DictionaryEntry | null {
    if (!row.term) return null;
    const shared = all.some(
      (e) => e.id !== row.main.id && isLearned(e) && e.kind === "replacement" && e.status === "active" && lower(e.toText) === lower(row.term!.toText),
    );
    return shared ? null : row.term;
  }

  async function setLearned(row: LearnedRow, status: "active" | "disabled") {
    const targets = [row.main, termGoesWith(row)].filter((x): x is DictionaryEntry => x !== null);
    const before: StatusChange[] = targets.map((t) => ({ id: t.id, status: statusOf(t) }));
    const wasCandidate = before[0].status === "candidate";
    const ok = await applyStatuses(
      targets.map((t) => ({ id: t.id, status })),
      before,
    );
    if (!ok) return fail("change that");
    const what = row.main.kind === "replacement" ? `“${row.main.fromText}” → “${row.main.toText}”` : `“${row.main.toText}”`;
    if (status === "disabled") {
      toast.show(`Undone. Wren won't learn ${what} again.`, {
        action: {
          label: "Undo",
          onClick: () =>
            void applyStatuses(before, targets.map((t) => ({ id: t.id, status }))).then((back) => back || fail("put that back")),
        },
      });
    } else {
      toast.show(wasCandidate ? `Kept: ${what}` : `Back on: ${what}`);
    }
  }

  async function removeLearned(row: LearnedRow) {
    const targets = [row.main, termGoesWith(row)].filter((x): x is DictionaryEntry => x !== null);
    try {
      await Promise.all(targets.map((t) => dictionaryDelete(t.id)));
      drop(targets.map((t) => t.id));
      toast.show("Deleted. Wren may learn it again if you make the same fix.");
    } catch {
      fail("delete that");
      void load();
    }
  }

  const tabs = [
    { id: "words" as const, label: "Words", count: words.length, icon: <BookA size={14} strokeWidth={2} /> },
    { id: "replacements" as const, label: "Replacements", count: replacements.length, icon: <ArrowLeftRight size={14} strokeWidth={2} /> },
    { id: "learned" as const, label: "Learned", count: learned.length, icon: <Sparkles size={14} strokeWidth={2} /> },
  ];

  return (
    <div className="page page--dictionary">
      <header className="page-head">
        <div>
          <h1 className="page__title">Dictionary</h1>
          <p className="page__lede">Keep the names, phrases and spellings you use. Wren learns from your fixes too.</p>
        </div>
      </header>

      <div className="dictionary-content" data-tab={tab}>
        <Tabs items={tabs} value={tab} onChange={setTab} label="Dictionary lists" idPrefix="dict" />

        <div className="tabpanel" role="tabpanel" id={`dict-panel-${tab}`} aria-labelledby={`dict-tab-${tab}`} key={tab}>
          {entries === null ? (
            failed ? <LoadError compact what="your dictionary" onRetry={() => void load()} /> : null
          ) : tab === "words" ? (
            <>
              <AddWord existing={all} onAdd={addWord} />
              {words.length === 0 ? (
                <EmptyState compact icon={<BookA size={20} strokeWidth={2} />} title="No words yet">
                  Add names, product names and jargon you say often. Wren will listen out for them and spell them your way.
                </EmptyState>
              ) : (
                <ul className="dict-list" aria-label="Words">
                  {words.map((w) => (
                    <li key={w.id} className={`wordchip ${w.status === "disabled" ? "is-muted" : ""}`} title={usedLabel(w.hits)}>
                      <span className="tinted-icon" aria-hidden="true">
                        <BookA size={15} strokeWidth={2} />
                      </span>
                      <div className="wordchip__body">
                        <span className="wordchip__text">{w.toText}</span>
                        <span className="wordchip__meta num">{w.hits > 0 ? usedLabel(w.hits) : "New"}</span>
                      </div>
                      <IconButton label={`Remove ${w.toText}`} tone="danger" onClick={() => void removeWord(w)}>
                        <Trash2 size={15} strokeWidth={1.9} />
                      </IconButton>
                    </li>
                  ))}
                </ul>
              )}
            </>
          ) : tab === "replacements" ? (
            <>
              <AddReplacement existing={all} onAdd={addReplacement} />
              {replacements.length === 0 ? (
                <EmptyState compact icon={<ArrowLeftRight size={20} strokeWidth={2} />} title="No replacements yet">
                  When Wren hears one thing and you want another, add it here. For example “pie test” → “pytest”.
                </EmptyState>
              ) : (
                <ul className="dlist" aria-label="Replacements">
                  {replacements.map((r) => (
                    <Row key={r.id} icon={<ArrowLeftRight size={15} strokeWidth={2} />} muted={r.status === "disabled"}>
                      <div className="drow__main">
                        <Swap from={r.fromText ?? ""} to={r.toText} />
                      </div>
                      <span className="drow__meta num">{r.status === "disabled" ? "Off" : usedLabel(r.hits)}</span>
                      <div className="drow__actions">
                        <Toggle checked={r.status !== "disabled"} onChange={(on) => void setOn(r, on)} label={`Use ${r.fromText} → ${r.toText}`} />
                        <DeleteButton label={`Delete ${r.fromText} → ${r.toText}`} onDelete={() => void removeReplacement(r)} />
                      </div>
                    </Row>
                  ))}
                </ul>
              )}
            </>
          ) : (
            <>
              <p className="tab-intro">
                Wren learns when you fix a word in History, and when you correct a word right after it pastes. Undo stops it
                for good; Delete lets Wren learn it again.
              </p>
              {learned.length === 0 ? (
                <EmptyState compact icon={<Sparkles size={20} strokeWidth={2} />} title="Nothing learned yet">
                  Fix a word in History, or correct one straight after Wren pastes it, and it will show up here.
                </EmptyState>
              ) : (
                <ul className="dlist dlist--learned" aria-label="Learned">
                  {learned.map((row) => {
                    const e = row.main;
                    const off = e.status === "disabled";
                    const waiting = e.status === "candidate";
                    return (
                      <Row key={e.id} icon={<Sparkles size={15} strokeWidth={2} />} muted={off}>
                        <div className="drow__main">
                          {e.kind === "replacement" ? <Swap from={e.fromText ?? ""} to={e.toText} /> : <span className="swap swap__to">{e.toText}</span>}
                          <div className="drow__tags">
                            <span className="chip chip--plain">{e.source === "learnedHistory" ? "Fixed in History" : "From your edit"}</span>
                            {e.kind === "term" && <span className="chip chip--plain">Word</span>}
                            {row.term && e.kind === "replacement" && <span className="chip chip--plain">Added to Words too</span>}
                            {waiting && <span className="chip chip--quiet">Seen once · needs one more</span>}
                            {off && <span className="chip chip--plain">Undone</span>}
                          </div>
                        </div>
                        <span className="drow__meta num">{waiting || off ? "" : usedLabel(e.hits)}</span>
                        <div className="drow__actions">
                          {off ? (
                            <Button size="sm" variant="ghost" icon={<RotateCcw size={14} strokeWidth={2.2} />} onClick={() => void setLearned(row, "active")}>
                              Turn back on
                            </Button>
                          ) : waiting ? (
                            <>
                              <Button size="sm" variant="secondary" icon={<Check size={14} strokeWidth={2.4} />} onClick={() => void setLearned(row, "active")}>
                                Keep
                              </Button>
                              <Button size="sm" variant="ghost" icon={<Undo2 size={14} strokeWidth={2.2} />} onClick={() => void setLearned(row, "disabled")}>
                                Undo
                              </Button>
                            </>
                          ) : (
                            <Button size="sm" variant="ghost" icon={<Undo2 size={14} strokeWidth={2.2} />} onClick={() => void setLearned(row, "disabled")}>
                              Undo
                            </Button>
                          )}
                          <DeleteButton
                            label={e.kind === "replacement" ? `Delete ${e.fromText} → ${e.toText}` : `Delete ${e.toText}`}
                            onDelete={() => void removeLearned(row)}
                          />
                        </div>
                      </Row>
                    );
                  })}
                </ul>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
