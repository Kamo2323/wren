// Stats: totals (today, this week, all time), words per day for 30 days, words by hour of day with the
// busiest hour picked out, and speaking speed against typing. All from `stats_get` (spec §9), which reads
// `hourly_totals`, so nothing here disappears with the 72-hour sweep.
import { useCallback, useEffect, useState, type ReactNode } from "react";
import type { Stats as StatsData } from "../lib/types";
import { onHistoryChanged, statsGet, subscribe } from "../lib/api";
import { formatNumber, formatSavedText, hourLabel } from "../lib/format";
import { useConstants } from "../lib/useConstants";
import { BarChart } from "../components/BarChart";
import { LoadError } from "../components/Empty";
import { LineChart } from "../components/LineChart";
import { dayWpm } from "../lib/chart";
import { CalendarDays, Clock, Flame, Gauge, MessageSquareText, Sparkles, Timer, Zap } from "../components/Icon";

function parseDate(date: string): Date {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d);
}
const longDay = (date: string) => parseDate(date).toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short" });
const shortDay = (date: string) => parseDate(date).toLocaleDateString("en-GB", { day: "numeric", month: "short" });
const hourSpan = (h: number) => `${hourLabel(h)} – ${hourLabel((h + 1) % 24)}`;
const plural = (n: number, one: string, many: string) => `${formatNumber(n)} ${n === 1 ? one : many}`;

// ---------------------------------------------------------------- totals

function Mini({ icon, children }: { icon: ReactNode; children: ReactNode }) {
  return (
    <span className="mini">
      <span className="mini__icon">{icon}</span>
      {children}
    </span>
  );
}

function Total({ label, icon, words, children, hero }: { label: string; icon: ReactNode; words: number; children: ReactNode; hero?: boolean }) {
  return (
    <div className={`total ${hero ? "total--hero" : ""} ${words === 0 ? "is-muted" : ""}`}>
      <div className="stat__label">
        <span className="stat__icon">{icon}</span>
        {label}
      </div>
      <div className="total__value num">
        <span className="total__num">{formatNumber(words)}</span>
        <span className="stat__unit">{words === 1 ? "word" : "words"}</span>
      </div>
      <div className="total__minis">{children}</div>
    </div>
  );
}

function Totals({ s }: { s: StatsData }) {
  const savedAll = formatSavedText(s.timeSavedMsAll);
  const savedWeek = formatSavedText(s.timeSavedMsWeek);
  return (
    <div className="totals">
      <Total label="Today" icon={<CalendarDays size={17} strokeWidth={2} />} words={s.wordsToday}>
        <Mini icon={<Gauge size={15} strokeWidth={2} />}>{s.wpmToday != null ? `${Math.round(s.wpmToday)} wpm` : "No speed yet"}</Mini>
        <Mini icon={<Flame size={15} strokeWidth={2} />}>{s.streakDays > 0 ? `${plural(s.streakDays, "day", "days")} in a row` : "No streak yet"}</Mini>
      </Total>
      <Total label="This week" icon={<Zap size={17} strokeWidth={2} />} words={s.wordsWeek}>
        <Mini icon={<Timer size={15} strokeWidth={2} />}>{s.timeSavedMsWeek > 0 ? `${savedWeek} saved` : "Nothing saved yet"}</Mini>
        <Mini icon={<CalendarDays size={15} strokeWidth={2} />}>Since Monday</Mini>
      </Total>
      <Total label="All time" icon={<Sparkles size={17} strokeWidth={2} />} words={s.wordsAll} hero>
        <Mini icon={<Timer size={15} strokeWidth={2} />}>{s.timeSavedMsAll > 0 ? `${savedAll} saved` : "Nothing saved yet"}</Mini>
        <Mini icon={<MessageSquareText size={15} strokeWidth={2} />}>{plural(s.dictationsAll, "dictation", "dictations")}</Mini>
      </Total>
    </div>
  );
}

// ---------------------------------------------------------------- charts

function DailyCard({ s }: { s: StatsData }) {
  const days = s.daily;
  const values = days.map((d) => d.words);
  const total = values.reduce((a, b) => a + b, 0);
  const active = values.filter((v) => v > 0).length;
  const avg = days.length ? Math.round(total / days.length) : 0;
  const best = values.reduce((bi, v, i) => (v > values[bi] ? i : bi), 0);
  const last = days.length - 1;
  const ticks = [0, 7, 14, 21]
    .filter((i) => i < last - 3)
    .map((i) => ({ i, label: shortDay(days[i].date) }))
    .concat(last >= 0 ? [{ i: last, label: "Today" }] : []);

  return (
    <section className="card chart-card" aria-labelledby="daily-title">
      <div className="chart-card__head">
        <div>
          <h2 id="daily-title" className="chart-card__title">Words per day</h2>
          <p className="chart-card__sub">Last 30 days</p>
        </div>
        <dl className="facts">
          <div className="fact">
            <dt>Total</dt>
            <dd className="num">{formatNumber(total)}</dd>
          </div>
          <div className="fact">
            <dt>Best day</dt>
            <dd className="num">{total > 0 ? formatNumber(values[best]) : "–"}</dd>
            {total > 0 && <span className="fact__note">{best === last ? "Today" : longDay(days[best].date)}</span>}
          </div>
          <div className="fact">
            <dt>Days used</dt>
            <dd className="num">
              {active}
              <span className="fact__of">/{days.length}</span>
            </dd>
          </div>
        </dl>
      </div>
      <BarChart
        values={values}
        accent={last}
        tipLabel={(i) => (i === last ? `Today, ${shortDay(days[i].date)}` : longDay(days[i].date))}
        xTicks={ticks}
        reference={avg > 0 ? { value: avg, label: `Average ${formatNumber(avg)}` } : null}
        height={230}
        ariaLabel={`Words per day for the last 30 days: ${formatNumber(total)} in total, ${active} days used.`}
        emptyText="Your first dictation starts this chart."
        className="chart--daily"
      />
    </section>
  );
}

function HourlyCard({ s }: { s: StatsData }) {
  const values = s.hourly;
  const busiest = s.busiestHour;
  const total = values.reduce((a, b) => a + b, 0);
  const ticks = [0, 6, 12, 18].map((i) => ({ i, label: hourLabel(i) }));
  const share = busiest != null && total > 0 ? Math.round((values[busiest] / total) * 100) : 0;

  return (
    <section className="card chart-card" aria-labelledby="hourly-title">
      <div className="chart-card__head">
        <div>
          <h2 id="hourly-title" className="chart-card__title">Time of day</h2>
          <p className="chart-card__sub">Words by hour, last 30 days</p>
        </div>
      </div>
      <div className="headline">
        <span className="headline__icon">
          <Clock size={18} strokeWidth={2} />
        </span>
        {busiest != null ? (
          <div>
            <div className="headline__value num">{hourSpan(busiest)}</div>
            <div className="headline__caption">
              Your busiest hour · {plural(values[busiest] ?? 0, "word", "words")} ({share}%)
            </div>
          </div>
        ) : (
          <div>
            <div className="headline__value is-muted">No busiest hour yet</div>
            <div className="headline__caption">It shows up after a few dictations.</div>
          </div>
        )}
      </div>
      <BarChart
        values={values}
        accent={busiest}
        tipLabel={(i) => hourSpan(i)}
        xTicks={ticks}
        height={150}
        ariaLabel={busiest != null ? `Words by hour of day. Busiest hour: ${hourSpan(busiest)}.` : "Words by hour of day: none yet."}
        emptyText="Nothing yet."
        className="chart--hourly"
      />
    </section>
  );
}

function SpeedCard({ s, typingWpm }: { s: StatsData; typingWpm: number }) {
  const all = s.wpmAll != null ? Math.round(s.wpmAll) : null;
  const factor = all != null && typingWpm > 0 ? all / typingWpm : null;
  const days = s.daily;
  const series = days.map((d) => dayWpm(d.words, d.speakingMs));
  const last = days.length - 1;
  const spoken = series.filter((v): v is number => v != null);
  const ticks = [0, 7, 14, 21]
    .filter((i) => i < last - 3)
    .map((i) => ({ i, label: shortDay(days[i].date) }))
    .concat(last >= 0 ? [{ i: last, label: "Today" }] : []);

  // trend: the average of the last 7 days with speech against the 7 before them
  let trend: string | null = null;
  if (spoken.length >= 6) {
    const recent = series.slice(-7).filter((v): v is number => v != null);
    const before = series.slice(-14, -7).filter((v): v is number => v != null);
    if (recent.length >= 2 && before.length >= 2) {
      const avg = (xs: number[]) => xs.reduce((a, b) => a + b, 0) / xs.length;
      const delta = (avg(recent) - avg(before)) / avg(before);
      trend = Math.abs(delta) < 0.02 ? "Steady for two weeks" : `${Math.round(Math.abs(delta) * 100)}% ${delta > 0 ? "up" : "down"} on the week before`;
    }
  }

  return (
    <section className="card chart-card" aria-labelledby="speed-title">
      <div className="chart-card__head">
        <div>
          <h2 id="speed-title" className="chart-card__title">Speaking speed</h2>
          <p className="chart-card__sub">Words per minute, last 30 days</p>
        </div>
      </div>
      <div className="headline">
        <span className="headline__icon">
          <Gauge size={18} strokeWidth={2} />
        </span>
        {all != null ? (
          <div>
            <div className="headline__value num">
              {all}
              <span className="headline__unit">wpm on average</span>
            </div>
            <div className="headline__caption">
              {factor != null && factor > 1.05 ? `${factor.toFixed(1)}× faster than typing` : "Words ÷ minutes spoken"}
              {trend ? ` · ${trend}` : ""}
            </div>
          </div>
        ) : (
          <div>
            <div className="headline__value is-muted">No speed yet</div>
            <div className="headline__caption">Words ÷ minutes spoken, once you dictate.</div>
          </div>
        )}
      </div>
      <LineChart
        values={series}
        accent={series[last] != null ? last : null}
        tipLabel={(i) => (i === last ? `Today, ${shortDay(days[i].date)}` : longDay(days[i].date))}
        tipValue={(i) => `${Math.round(series[i] ?? 0)} wpm`}
        xTicks={ticks}
        reference={{ value: typingWpm, label: `Typing ${typingWpm}` }}
        height={132}
        ariaLabel={
          spoken.length
            ? `Words per minute per day for the last 30 days, from ${Math.round(Math.min(...spoken))} to ${Math.round(Math.max(...spoken))}; typing speed ${typingWpm}.`
            : "Words per minute per day: none yet."
        }
        emptyText="Nothing spoken yet."
        className="chart--speed"
      />
    </section>
  );
}

// ---------------------------------------------------------------- page

/** The stats last shown, kept for this app session so a revisit paints at once. */
let lastStats: StatsData | null = null;

export default function Stats({ typingWpm: typingWpmSetting }: { typingWpm?: number }) {
  const constants = useConstants();
  const typingWpm = typingWpmSetting ?? constants.typingWpmDefault;
  const [s, setS] = useState<StatsData | null>(lastStats);
  const [failed, setFailed] = useState(false);

  const load = useCallback(async () => {
    try {
      const next = await statsGet();
      lastStats = next;
      setS(next);
      setFailed(false);
    } catch {
      setFailed(true); // keep any stats already on screen; say so only when there are none
    }
  }, []);

  useEffect(() => {
    void load();
    const unlisten = subscribe(onHistoryChanged(() => void load()));
    const onFocus = () => void load();
    window.addEventListener("focus", onFocus);
    return () => {
      unlisten();
      window.removeEventListener("focus", onFocus);
    };
  }, [load]);

  return (
    <div className="page page--stats">
      <header className="page-head">
        <div>
          <h1 className="page__title">Stats</h1>
          <p className="page__lede">How much Wren has typed for you. Kept for good, even after dictations are deleted.</p>
        </div>
      </header>

      {!s && failed && <LoadError what="your stats" onRetry={() => void load()} />}
      {s && (
        <>
          <Totals s={s} />
          <DailyCard s={s} />
          <div className="stats-pair">
            <HourlyCard s={s} />
            <SpeedCard s={s} typingWpm={typingWpm} />
          </div>
          <p className="stats-foot">
            Time saved = words ÷ your typing speed ({typingWpm} wpm, set in Settings), minus the time spent speaking. Speed =
            words ÷ minutes spoken.
          </p>
        </>
      )}
    </div>
  );
}
