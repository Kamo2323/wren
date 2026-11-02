// History: search box over everything said in the retention window (72 h), grouped by day, with the
// same cards as Home (click copies, Fix a word, Delete). Matches are marked in the text.
import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import type { TranscriptionRow } from "../lib/types";
import { historyRecent, historySearch, onHistoryChanged, subscribe } from "../lib/api";
import { formatNumber, retentionLabel } from "../lib/format";
import { useConstants } from "../lib/useConstants";
import { useDayGroups, useTranscriptActions } from "../lib/useTranscriptActions";
import { TranscriptCard } from "../components/TranscriptCard";
import { ChordKeys, EmptyState, LoadError } from "../components/Empty";
import { Button } from "../components/Button";
import { Clock, Search, X } from "../components/Icon";

/** Far more than 72 hours of dictation can hold, so in practice "everything that's kept". */
const ALL_LIMIT = 5000;
const SEARCH_DEBOUNCE_MS = 140;

export default function History() {
  const constants = useConstants();
  const kept = retentionLabel(constants.retentionHours);
  const [query, setQuery] = useState("");
  const [rows, setRows] = useState<TranscriptionRow[] | null>(null);
  const [searchedFor, setSearchedFor] = useState(""); // the query the rows on screen belong to
  const [now, setNow] = useState(() => Date.now());
  const [failed, setFailed] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const requestId = useRef(0);
  const { onCopy, onDelete, onFix } = useTranscriptActions(setRows);

  const load = useCallback(async (q: string) => {
    const id = ++requestId.current;
    const trimmed = q.trim();
    let result: TranscriptionRow[];
    try {
      result = await (trimmed ? historySearch(trimmed) : historyRecent(ALL_LIMIT));
    } catch {
      if (id !== requestId.current) return;
      setFailed(true);
      setRows(null);
      setSearchedFor(trimmed);
      return;
    }
    if (id !== requestId.current) return; // a newer search has started
    setFailed(false);
    setRows(result);
    setSearchedFor(trimmed);
    setNow(Date.now());
  }, []);

  // search as you type (debounced); empty query = everything
  useEffect(() => {
    const t = window.setTimeout(() => void load(query), query.trim() ? SEARCH_DEBOUNCE_MS : 0);
    return () => window.clearTimeout(t);
  }, [query, load]);

  // new dictations, window focus, and the clock
  const queryRef = useRef(query);
  queryRef.current = query;
  useEffect(() => {
    const reload = () => void load(queryRef.current);
    const unlisten = subscribe(onHistoryChanged(reload));
    window.addEventListener("focus", reload);
    const tick = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => {
      unlisten();
      window.removeEventListener("focus", reload);
      window.clearInterval(tick);
    };
  }, [load]);

  // Ctrl+F or "/" jumps to the search box
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
      if ((e.ctrlKey && e.key.toLowerCase() === "f") || (e.key === "/" && !typing)) {
        e.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function onSearchKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "Escape" && query) {
      e.preventDefault();
      setQuery("");
    }
  }

  const groups = useDayGroups(rows, now);
  const searching = searchedFor.length > 0;
  const words = (rows ?? []).reduce((a, r) => a + r.words, 0);

  return (
    <div className="page page--history">
      <header className="page-head">
        <div>
          <h1 className="page__title">History</h1>
          <p className="page__lede">Everything you said in the last {kept}. Click a card to copy it.</p>
        </div>
        <span className="keep-note">
          <Clock size={15} strokeWidth={2} /> Deleted after {kept}
        </span>
      </header>

      <div className="search">
        <Search className="search__icon" size={19} strokeWidth={2} aria-hidden="true" />
        <input
          ref={inputRef}
          className="search__input"
          type="search"
          value={query}
          placeholder="Search what you said"
          aria-label="Search history"
          spellCheck={false}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onSearchKey}
        />
        {query ? (
          <button type="button" className="search__clear" aria-label="Clear search" title="Clear search" onClick={() => { setQuery(""); inputRef.current?.focus(); }}>
            <X size={16} strokeWidth={2.2} />
          </button>
        ) : (
          <kbd className="search__kbd" aria-hidden="true">Ctrl F</kbd>
        )}
      </div>

      {rows !== null && rows.length > 0 && (
        <p className="results-line num" aria-live="polite">
          {searching ? (
            <>
              <strong>{formatNumber(rows.length)}</strong> {rows.length === 1 ? "match" : "matches"} for “{searchedFor}”
            </>
          ) : (
            <>
              <strong>{formatNumber(rows.length)}</strong> {rows.length === 1 ? "dictation" : "dictations"} ·{" "}
              <strong>{formatNumber(words)}</strong> words
            </>
          )}
        </p>
      )}

      {failed ? (
        <LoadError what={searchedFor ? "search results" : "your history"} onRetry={() => void load(query)} />
      ) : rows === null ? null : rows.length === 0 ? (
        searching ? (
          <EmptyState
            icon={<Search size={22} strokeWidth={2} />}
            title={`Nothing matches “${searchedFor}”`}
            action={
              <Button variant="secondary" onClick={() => { setQuery(""); inputRef.current?.focus(); }}>
                Clear search
              </Button>
            }
          >
            Search looks through everything you said in the last {kept}. Older dictations are already gone.
          </EmptyState>
        ) : (
          <EmptyState art={<ChordKeys />} title={`Nothing from the last ${kept}`} note="Your stats are kept for good, even after dictations are deleted.">
            Hold the left Ctrl and Windows keys and speak. Everything you dictate is kept here for {kept}, so you can
            search it and copy it again.
          </EmptyState>
        )
      ) : (
        <div className="history-list">
          {groups.map((g) => (
            <section className="day" key={g.label} aria-label={g.label}>
              <h3 className="day__label">
                {g.label}
                <span className="day__count num">{g.rows.length}</span>
              </h3>
              <div className="day__cards">
                {g.rows.map((r) => (
                  <TranscriptCard
                    key={r.id}
                    row={r}
                    now={now}
                    highlight={searching ? searchedFor : undefined}
                    onCopy={onCopy}
                    onDelete={onDelete}
                    onFix={onFix}
                  />
                ))}
              </div>
            </section>
          ))}
        </div>
      )}
    </div>
  );
}
