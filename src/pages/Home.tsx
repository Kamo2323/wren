// Home: four big stat tiles, the 30-day strip, then every recent dictation as a big card.
// Click a card to copy it.
import { useCallback, useEffect, useMemo, useState } from "react";
import type { Stats, TranscriptionRow } from "../lib/types";
import { copyText, historyDelete, historyFixWord, historyRecent, onHistoryChanged, replaceWord, statsGet } from "../lib/api";
import { dayLabel, formatNumber, formatSaved, hourLabel } from "../lib/format";
import { StatTile } from "../components/StatTile";
import { DayStrip } from "../components/DayStrip";
import { TranscriptCard } from "../components/TranscriptCard";
import { useToast } from "../components/Toast";
import { Check, Clock, Flame, Gauge, Timer } from "../components/Icon";

const RECENT_LIMIT = 100;

function StatsRow({ stats, typingWpm }: { stats: Stats | null; typingWpm: number }) {
  if (!stats) return <div className="stats stats--loading" aria-hidden="true" />;
  const saved = formatSaved(stats.timeSavedMsAll);
  const week = formatSaved(stats.timeSavedMsWeek);
  const wpm = stats.wpmAll != null ? Math.round(stats.wpmAll) : null;
  const factor = wpm != null && typingWpm > 0 ? wpm / typingWpm : null;
  const hour = stats.busiestHour;
  const empty = stats.dictationsAll === 0;

  return (
    <div className="stats">
      <StatTile
        icon={<Timer size={17} strokeWidth={2} />}
        label="Time saved"
        value={saved.value}
        unit={saved.unit}
        caption={empty ? "Starts when you dictate" : stats.timeSavedMsWeek > 0 ? `${week.value} ${week.unit} this week` : "None yet this week"}
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

function EmptyRecent() {
  return (
    <div className="empty">
      <div className="empty__keys" aria-hidden="true">
        <kbd className="key">Ctrl</kbd>
        <span className="empty__plus">+</span>
        <kbd className="key">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
            <rect x="1" y="1" width="6.5" height="6.5" rx="1" />
            <rect x="8.5" y="1" width="6.5" height="6.5" rx="1" />
            <rect x="1" y="8.5" width="6.5" height="6.5" rx="1" />
            <rect x="8.5" y="8.5" width="6.5" height="6.5" rx="1" />
          </svg>
          Win
        </kbd>
      </div>
      <h3 className="empty__title">Hold, speak, let go</h3>
      <p className="empty__text">
        Hold the left Ctrl and Windows keys while you talk. When you let go, Wren types it where your cursor is.
        Double-tap them for hands-free.
      </p>
      <p className="empty__note">Your dictations show up here for 3 days. Click one to copy it again.</p>
    </div>
  );
}

export default function Home({ typingWpm = 52 }: { typingWpm?: number }) {
  const [stats, setStats] = useState<Stats | null>(null);
  const [rows, setRows] = useState<TranscriptionRow[] | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const toast = useToast();

  const load = useCallback(async () => {
    const [s, r] = await Promise.all([statsGet().catch(() => null), historyRecent(RECENT_LIMIT).catch(() => [])]);
    if (s) setStats(s);
    setRows(r);
    setNow(Date.now());
  }, []);

  useEffect(() => {
    void load();
    let unlisten: (() => void) | undefined;
    void onHistoryChanged(() => void load()).then((u) => (unlisten = u));
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    const tick = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => {
      unlisten?.();
      window.removeEventListener("focus", onFocus);
      window.clearInterval(tick);
    };
  }, [load]);

  const groups = useMemo(() => {
    const out: Array<{ label: string; rows: TranscriptionRow[] }> = [];
    for (const r of rows ?? []) {
      const label = dayLabel(r.createdAtUtc);
      const g = out[out.length - 1];
      if (g && g.label === label) g.rows.push(r);
      else out.push({ label, rows: [r] });
    }
    return out;
  }, [rows]);

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
      } catch {
        toast.show("Couldn't delete that one.", { tone: "danger" });
      }
    },
    [toast],
  );

  const onFix = useCallback(
    async (row: TranscriptionRow, from: string, to: string) => {
      try {
        await historyFixWord(row.id, from, to);
        setRows((rs) => (rs ?? []).map((r) => (r.id === row.id ? { ...r, finalText: replaceWord(r.finalText, from, to) } : r)));
        toast.show(`Fixed. Wren will write “${to}” from now on.`, { tone: "success", icon: <Check size={16} strokeWidth={2.6} />, ms: 3200 });
      } catch {
        toast.show("Couldn't save that fix.", { tone: "danger" });
        throw new Error("fix failed");
      }
    },
    [toast],
  );

  return (
    <div className="page page--home">
      <StatsRow stats={stats} typingWpm={typingWpm} />
      {stats && <DayStrip days={stats.daily} />}

      <section className="recent" aria-label="Recent dictations">
        <div className="section-head">
          <h2 className="section-head__title">Recent</h2>
          {rows && rows.length > 0 && (
            <span className="section-head__note">
              {formatNumber(rows.length)} in the last 3 days · click one to copy it
            </span>
          )}
        </div>

        {rows === null ? null : rows.length === 0 ? (
          <EmptyRecent />
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
