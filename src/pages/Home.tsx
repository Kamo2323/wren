// Home: four big stat tiles, the 30-day strip, then every recent dictation as a big card.
// Click a card to copy it.
import { useCallback, useEffect, useState } from "react";
import type { Stats, TranscriptionRow } from "../lib/types";
import { historyRecent, mainReady, onHistoryChanged, statsGet, subscribe } from "../lib/api";
import { formatNumber, formatSaved, formatSavedShort, hourLabel, retentionLabel } from "../lib/format";
import { useDayGroups, useNewIds, useTranscriptActions } from "../lib/useTranscriptActions";
import { useConstants } from "../lib/useConstants";
import { SectionHeader } from "../components/Card";
import { StatTile } from "../components/StatTile";
import { DayStrip, DayStripPlaceholder } from "../components/DayStrip";
import { ChordKeys, EmptyState } from "../components/Empty";
import { TranscriptCard } from "../components/TranscriptCard";
import { Clock, Flame, Gauge, Timer } from "../components/Icon";

const RECENT_LIMIT = 100;

// The last data shown, kept for this app session: a revisit paints complete at once and refreshes
// in the background.
let lastStats: Stats | null = null;
let lastRows: TranscriptionRow[] | null = null;

function StatsRow({ stats, typingWpm }: { stats: Stats | null; typingWpm: number }) {
  if (!stats) return <div className="stats stats--loading" aria-hidden="true" />;
  const wpm = stats.wpmAll != null ? Math.round(stats.wpmAll) : null;
  const factor = wpm != null && typingWpm > 0 ? wpm / typingWpm : null;
  const hour = stats.busiestHour;
  const empty = stats.dictationsAll === 0;

  return (
    <div className="stats">
      <StatTile
        icon={<Timer size={17} strokeWidth={2} />}
        label="Time saved"
        parts={formatSaved(stats.timeSavedMsAll)}
        caption={empty ? "Starts when you dictate" : stats.timeSavedMsWeek > 0 ? `${formatSavedShort(stats.timeSavedMsWeek)} this week` : "None yet this week"}
        explain={`Words ÷ your typing speed (${typingWpm} wpm), minus the time spent speaking.`}
        muted={empty}
      />
      <StatTile
        icon={<Gauge size={17} strokeWidth={2} />}
        label="Speed"
        value={wpm != null ? String(wpm) : "–"}
        unit={wpm != null ? "wpm" : undefined}
        caption={
          factor != null && factor > 1.05
            ? `${factor.toFixed(1)}× typing speed`
            : stats.wpmToday != null
              ? `${Math.round(stats.wpmToday)} wpm today`
              : "Shows after your first dictation"
        }
        explain="Words ÷ minutes spent speaking, across all dictations."
        muted={wpm == null}
      />
      <StatTile
        icon={<Flame size={17} strokeWidth={2} />}
        label="Day streak"
        value={String(stats.streakDays)}
        unit={stats.streakDays === 1 ? "day" : "days"}
        caption={stats.streakDays > 0 ? "Keep it going" : "Start one today"}
        explain="Days in a row with at least one dictation, ending today or yesterday."
        muted={stats.streakDays === 0}
      />
      <StatTile
        icon={<Clock size={17} strokeWidth={2} />}
        label="Busiest hour"
        value={hour != null ? hourLabel(hour).split(" ")[0] : "–"}
        unit={hour != null ? hourLabel(hour).split(" ")[1] : undefined}
        caption={hour != null ? "Last 30 days" : "Appears as you dictate"}
        explain="The hour of the day with the most words over the last 30 days."
        muted={hour == null}
      />
    </div>
  );
}

function EmptyRecent({ kept }: { kept: string }) {
  return (
    <EmptyState art={<ChordKeys />} title="Hold, speak, let go" note={`Your dictations show up here for ${kept}. Click one to copy it again.`}>
      Hold the left Ctrl and Windows keys while you talk. When you let go, Wren types it where your cursor is.
      Double-tap them for hands-free.
    </EmptyState>
  );
}

/** `typingWpm` is the user's setting; until it loads, the default from the shared constants is used. */
export default function Home({ typingWpm: typingWpmSetting }: { typingWpm?: number }) {
  const constants = useConstants();
  const typingWpm = typingWpmSetting ?? constants.typingWpmDefault;
  const kept = retentionLabel(constants.retentionHours);
  const [stats, setStats] = useState<Stats | null>(lastStats);
  const [rows, setRows] = useState<TranscriptionRow[] | null>(lastRows);
  const [now, setNow] = useState(() => Date.now());
  const { onCopy, onDelete, onFix } = useTranscriptActions(setRows);

  const load = useCallback(async () => {
    const [s, r] = await Promise.all([statsGet().catch(() => null), historyRecent(RECENT_LIMIT).catch(() => [])]);
    if (s) setStats(s);
    setRows(r);
    setNow(Date.now());
    // The window is shown once this first screen has painted (Rust has a fallback).
    requestAnimationFrame(() => requestAnimationFrame(mainReady));
  }, []);

  useEffect(() => {
    if (stats) lastStats = stats;
  }, [stats]);
  useEffect(() => {
    if (rows) lastRows = rows;
  }, [rows]);

  useEffect(() => {
    void load();
    const unlisten = subscribe(onHistoryChanged(() => void load()));
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    const tick = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => {
      unlisten();
      window.removeEventListener("focus", onFocus);
      window.clearInterval(tick);
    };
  }, [load]);

  const groups = useDayGroups(rows, now);
  const newIds = useNewIds(rows, "home");

  return (
    <div className="page page--home">
      <StatsRow stats={stats} typingWpm={typingWpm} />
      {stats ? <DayStrip days={stats.daily} /> : <DayStripPlaceholder />}

      <section className="recent" aria-label="Recent dictations">
        <SectionHeader
          title="Recent"
          note={rows && rows.length > 0 ? `${formatNumber(rows.length)} in the last ${kept} · click one to copy it` : undefined}
        />

        {rows === null ? null : rows.length === 0 ? (
          <EmptyRecent kept={kept} />
        ) : (
          groups.map((g) => (
            <div className="day" key={g.label}>
              <h3 className="day__label">{g.label}</h3>
              <div className="day__cards">
                {g.rows.map((r) => (
                  <TranscriptCard key={r.id} row={r} now={now} onCopy={onCopy} onDelete={onDelete} onFix={onFix} isNew={newIds.has(r.id)} />
                ))}
              </div>
            </div>
          ))
        )}
      </section>
    </div>
  );
}
