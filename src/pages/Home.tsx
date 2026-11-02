// Home: four big stat tiles, the 30-day strip, then every recent dictation as a big card.
// Click a card to copy it.
import { useCallback, useEffect, useState } from "react";
import type { Stats, TranscriptionRow } from "../lib/types";
import { historyRecent, onHistoryChanged, statsGet, subscribe } from "../lib/api";
import { formatNumber, formatSaved, formatSavedText, hourLabel, retentionLabel } from "../lib/format";
import { useDayGroups, useTranscriptActions } from "../lib/useTranscriptActions";
import { useConstants } from "../lib/useConstants";
import { SectionHeader } from "../components/Card";
import { StatTile } from "../components/StatTile";
import { DayStrip } from "../components/DayStrip";
import { ChordKeys, EmptyState } from "../components/Empty";
import { TranscriptCard } from "../components/TranscriptCard";
import { Clock, Flame, Gauge, Timer } from "../components/Icon";

const RECENT_LIMIT = 100;

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
        caption={empty ? "Starts when you dictate" : stats.timeSavedMsWeek > 0 ? `${formatSavedText(stats.timeSavedMsWeek)} this week` : "None yet this week"}
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
            ? `${factor.toFixed(1)}× faster than typing`
            : stats.wpmToday != null
              ? `${Math.round(stats.wpmToday)} wpm today`
              : "Words ÷ minutes spoken"
        }
        explain="Words ÷ minutes spent speaking, across all dictations."
        muted={wpm == null}
      />
      <StatTile
        icon={<Flame size={17} strokeWidth={2} />}
        label="Day streak"
        value={String(stats.streakDays)}
        unit={stats.streakDays === 1 ? "day" : "days"}
        caption={stats.streakDays > 0 ? "Keep it going tomorrow" : "Dictate today to start one"}
        explain="Days in a row with at least one dictation, ending today or yesterday."
        muted={stats.streakDays === 0}
      />
      <StatTile
        icon={<Clock size={17} strokeWidth={2} />}
        label="Busiest hour"
        value={hour != null ? hourLabel(hour).split(" ")[0] : "–"}
        unit={hour != null ? hourLabel(hour).split(" ")[1] : undefined}
        caption={hour != null ? "Most words, last 30 days" : "Appears as you dictate"}
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
  const [stats, setStats] = useState<Stats | null>(null);
  const [rows, setRows] = useState<TranscriptionRow[] | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const { onCopy, onDelete, onFix } = useTranscriptActions(setRows);

  const load = useCallback(async () => {
    const [s, r] = await Promise.all([statsGet().catch(() => null), historyRecent(RECENT_LIMIT).catch(() => [])]);
    if (s) setStats(s);
    setRows(r);
    setNow(Date.now());
  }, []);

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

  return (
    <div className="page page--home">
      <StatsRow stats={stats} typingWpm={typingWpm} />
      {stats && <DayStrip days={stats.daily} />}

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
                  <TranscriptCard key={r.id} row={r} now={now} onCopy={onCopy} onDelete={onDelete} onFix={onFix} />
                ))}
              </div>
            </div>
          ))
        )}
      </section>
    </div>
  );
}
