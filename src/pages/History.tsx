// History: search box + day filter over everything said in the retention window (72 h), grouped
// by day, with subtle hairline cards, real app icons, inline audio playback, Fix a word and Delete.
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import type { TranscriptionRow } from "../lib/types";
import { historyAudioIds, historyRecent, historySearch, onHistoryChanged, subscribe } from "../lib/api";
import { dayLabel, formatNumber, retentionLabel } from "../lib/format";
import { useConstants } from "../lib/useConstants";
import { useDayGroups, useNewIds, useTranscriptActions } from "../lib/useTranscriptActions";
import { TranscriptCard } from "../components/TranscriptCard";
import { ChordKeys, EmptyState, LoadError } from "../components/Empty";
import { Button } from "../components/Button";
import { Clock, Search, X } from "../components/Icon";

const ALL_LIMIT = 5000;
const SEARCH_DEBOUNCE_MS = 140;

let lastAll: TranscriptionRow[] | null = null;

export default function History() {
  const constants = useConstants();
  const isMac = constants.platform === "macos";
  const kept = retentionLabel(constants.retentionHours);
  const [query, setQuery] = useState("");
  const [dayFilter, setDayFilter] = useState("all");
  const [rows, setRows] = useState<TranscriptionRow[] | null>(lastAll);
  const [audioIds, setAudioIds] = useState<ReadonlySet<string>>(() => new Set());
  const [playingId, setPlayingId] = useState<string | null>(null);
  const [searchedFor, setSearchedFor] = useState("");
  const [now, setNow] = useState(() => Date.now());
  const [failed, setFailed] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const requestId = useRef(0);
  const { onCopy, onDelete, onFix } = useTranscriptActions(setRows);
  const newIds = useNewIds(rows, "history");

  useEffect(() => {
    if (rows && !failed && searchedFor === "") lastAll = rows;
  }, [rows, failed, searchedFor]);

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
    if (id !== requestId.current) return;
    setFailed(false);
    setRows(result);
    setSearchedFor(trimmed);
    setNow(Date.now());
    if (result.length > 0) {
      void historyAudioIds(result.map((r) => r.id))
        .then((ids) => {
          if (id === requestId.current) setAudioIds(new Set(ids));
        })
        .catch(() => {});
    } else {
      setAudioIds(new Set());
    }
  }, []);

  useEffect(() => {
    const t = window.setTimeout(() => void load(query), query.trim() ? SEARCH_DEBOUNCE_MS : 0);
    return () => window.clearTimeout(t);
  }, [query, load]);

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

  // ⌘F / Ctrl+F or "/" jumps to the search box
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const typing = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement;
      if (((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "f") || (e.key === "/" && !typing)) {
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

  const availableDays = useMemo(() => {
    const seen = new Set<string>();
    const labels: string[] = [];
    const refDate = new Date(now);
    for (const r of rows ?? []) {
      const lbl = dayLabel(r.createdAtUtc, refDate);
      if (!seen.has(lbl)) {
        seen.add(lbl);
        labels.push(lbl);
      }
    }
    return labels;
  }, [rows, now]);

  const effectiveDay = dayFilter === "all" || availableDays.includes(dayFilter) ? dayFilter : "all";

  const visibleRows = useMemo(() => {
    if (!rows) return null;
    if (effectiveDay === "all") return rows;
    const refDate = new Date(now);
    return rows.filter((r) => dayLabel(r.createdAtUtc, refDate) === effectiveDay);
  }, [rows, effectiveDay, now]);

  const groups = useDayGroups(visibleRows, now);
  const searching = searchedFor.length > 0;
  const words = (visibleRows ?? []).reduce((a, r) => a + r.words, 0);

  return (
    <div className="page page--history">
      <header className="page-head">
        <div>
          <h1 className="page__title">History</h1>
          <p className="page__lede">
            Dictations and their recordings are kept on {isMac ? "this Mac" : "this PC"} for {kept}. Click a card to copy it.
          </p>
        </div>
        <span className="keep-note">
          <Clock size={15} strokeWidth={2} aria-hidden="true" /> Deleted after {kept}
        </span>
      </header>

      <div className="history-controls">
        <div className="search">
          <Search className="search__icon" size={16} strokeWidth={2} aria-hidden="true" />
          <input
            ref={inputRef}
            className="search__input"
            type="search"
            value={query}
            placeholder="Search what you said"
            aria-label="Search history"
            spellCheck={false}
            onChange={(e) => {
              setPlayingId(null);
              setQuery(e.target.value);
            }}
            onKeyDown={onSearchKey}
          />
          {query ? (
            <button
              type="button"
              className="search__clear"
              aria-label="Clear search"
              title="Clear search"
              onClick={() => {
                setQuery("");
                inputRef.current?.focus();
              }}
            >
              <X size={15} strokeWidth={2.2} />
            </button>
          ) : (
            <kbd className="search__kbd" aria-hidden="true">
              {isMac ? "⌘F" : "Ctrl F"}
            </kbd>
          )}
        </div>

        <select
          className="select history-day-select"
          aria-label="Filter by day"
          value={effectiveDay}
          onChange={(e) => {
            setPlayingId(null);
            setDayFilter(e.target.value);
          }}
        >
          <option value="all">All days</option>
          {availableDays.map((d) => (
            <option key={d} value={d}>
              {d}
            </option>
          ))}
        </select>
      </div>

      {visibleRows !== null && visibleRows.length > 0 && (
        <p className="results-line num" aria-live="polite">
          {searching ? (
            <>
              <strong>{formatNumber(visibleRows.length)}</strong> {visibleRows.length === 1 ? "match" : "matches"} for “{searchedFor}”
            </>
          ) : (
            <>
              <strong>{formatNumber(visibleRows.length)}</strong> {visibleRows.length === 1 ? "dictation" : "dictations"} ·{" "}
              <strong>{formatNumber(words)}</strong> words
            </>
          )}
        </p>
      )}

      {failed ? (
        <LoadError what={searchedFor ? "search results" : "your history"} onRetry={() => void load(query)} />
      ) : visibleRows === null ? null : visibleRows.length === 0 ? (
        searching || effectiveDay !== "all" ? (
          <EmptyState
            icon={<Search size={20} strokeWidth={2} />}
            title={searching ? `Nothing matches “${searchedFor}”` : "No dictations on this day"}
            action={
              <Button
                variant="secondary"
                onClick={() => {
                  setQuery("");
                  setDayFilter("all");
                  inputRef.current?.focus();
                }}
              >
                {searching ? "Clear search" : "Show all days"}
              </Button>
            }
          >
            Search looks through everything you said in the last {kept}. Older dictations are already gone.
          </EmptyState>
        ) : (
          <EmptyState
            art={<ChordKeys />}
            title={`Nothing from the last ${kept}`}
            note="Your stats are kept for good, even after dictations are deleted."
          >
            {isMac
              ? `Hold the fn and Control keys and speak. Everything you dictate is kept here for ${kept}, so you can search it and copy it again.`
              : `Hold the left Ctrl and Windows keys and speak. Everything you dictate is kept here for ${kept}, so you can search it and copy it again.`}
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
                    isNew={newIds.has(r.id)}
                    hasAudio={audioIds.has(r.id)}
                    playingId={playingId}
                    onPlayToggle={setPlayingId}
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
