// Stats: 3 semantic pastel-tinted summary tiles (teal Words dictated, violet Time saved, amber
// Current streak), violet Words per day bar chart with 7/30-day range toggle and dashed average,
// amber Time of day bars, and teal Speaking speed trend against your typing baseline.
import { useCallback, useEffect, useState } from "react";
import type { Stats as StatsData } from "../lib/types";
import { onHistoryChanged, statsGet, subscribe } from "../lib/api";
import { computeTimeSavedMs, formatNumber, formatSaved, formatSavedShort, hourLabel } from "../lib/format";
import { useConstants } from "../lib/useConstants";
import { BarChart } from "../components/BarChart";
import { LoadError } from "../components/Empty";
import { LineChart } from "../components/LineChart";
import { dayWpm } from "../lib/chart";
import { BookA, ChartColumn, Clock, Flame, Gauge } from "../components/Icon";

function parseDate(date: string): Date {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d);
}
const longDay = (date: string) => parseDate(date).toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short" });
const shortDay = (date: string) => parseDate(date).toLocaleDateString("en-GB", { day: "numeric", month: "short" });
const weekdayShort = (date: string) => parseDate(date).toLocaleDateString("en-GB", { weekday: "short" });
const hourSpan = (h: number) => `${hourLabel(h)} – ${hourLabel((h + 1) % 24)}`;
const plural = (n: number, one: string, many: string) => `${formatNumber(n)} ${n === 1 ? one : many}`;

function SummaryCards({ s, range, typingWpm }: { s: StatsData; range: 7 | 30; typingWpm: number }) {
  const slice = s.daily.slice(-range);
  const rangeWords = slice.reduce((a, d) => a + d.words, 0);
  const rangeSpeak = slice.reduce((a, d) => a + d.speakingMs, 0);
  const rangeSavedMs = computeTimeSavedMs(rangeWords, rangeSpeak, typingWpm);
  const savedParts = formatSaved(rangeSavedMs);

  return (
    <div className="stats-summary" role="group" aria-label={`Activity for the last ${range} days`}>
      <div className={`activity-metric ${rangeWords === 0 ? "is-muted" : ""}`} data-tone="teal">
        <span className="metric-label">
          <BookA size={15} strokeWidth={2} aria-hidden="true" />
          Words dictated
        </span>
        <strong className="metric num">{formatNumber(rangeWords)}</strong>
        <span className="hero-metric-period">
          Last {range} days · {formatNumber(s.wordsAll)} all time
        </span>
      </div>

      <div className={`activity-metric ${rangeSavedMs === 0 ? "is-muted" : ""}`} data-tone="violet">
        <span className="metric-label">
          <Clock size={15} strokeWidth={2} aria-hidden="true" />
          Time saved
        </span>
        <strong className="metric num">
          {savedParts.map((p, i) => (
            <span className="metric__part" key={i}>
              {p.value}
              <small>{p.unit}</small>
            </span>
          ))}
        </strong>
        <span className="hero-metric-period">
          {s.timeSavedMsAll > 0 ? `${formatSavedShort(s.timeSavedMsAll)} all time · ${plural(s.dictationsAll, "dictation", "dictations")}` : "Starts when you dictate"}
        </span>
      </div>

      <div className={`activity-metric ${s.streakDays === 0 ? "is-muted" : ""}`} data-tone="amber">
        <span className="metric-label">
          <Flame size={15} strokeWidth={2} aria-hidden="true" />
          Current streak
        </span>
        <strong className="metric num">
          {s.streakDays}
          <small>{s.streakDays === 1 ? "day" : "days"}</small>
        </strong>
        <span className="hero-metric-period">
          {s.wordsToday > 0 ? `${plural(s.wordsToday, "word", "words")} today` : s.streakDays > 0 ? "Keep it going" : "Start one today"}
        </span>
      </div>
    </div>
  );
}

function DailyCard({ s, range, onRange }: { s: StatsData; range: 7 | 30; onRange: (r: 7 | 30) => void }) {
  const days = s.daily.slice(-range);
  const values = days.map((d) => d.words);
  const total = values.reduce((a, b) => a + b, 0);
  const active = values.filter((v) => v > 0).length;
  const avg = days.length ? Math.round(total / days.length) : 0;
  const last = days.length - 1;
  const [hovered, setHovered] = useState<number | null>(null);

  const ticks =
    range === 7
      ? days.map((d, i) => ({ i, label: i === last ? "Today" : weekdayShort(d.date) }))
      : [0, 7, 14, 21]
          .filter((i) => i < last - 3)
          .map((i) => ({ i, label: shortDay(days[i].date) }))
          .concat(last >= 0 ? [{ i: last, label: "Today" }] : []);

  const focusIdx = hovered != null && hovered < days.length ? hovered : last;
  const focusLabel = focusIdx >= 0 ? (focusIdx === last ? `Today, ${shortDay(days[focusIdx].date)}` : longDay(days[focusIdx].date)) : "";
  const focusValue = focusIdx >= 0 ? plural(values[focusIdx], "word", "words") : "0 words";

  return (
    <section className="chart-card" data-chart="daily" aria-labelledby="daily-title">
      <div className="chart-card__head">
        <h2 id="daily-title" className="chart-card__title">
          <ChartColumn size={17} strokeWidth={2} aria-hidden="true" />
          Words per day
        </h2>
        <div className="segments" role="group" aria-label="Date range">
          {([7, 30] as const).map((r) => (
            <button
              key={r}
              type="button"
              className="segments__btn"
              aria-pressed={range === r}
              onClick={() => {
                setHovered(null);
                onRange(r);
              }}
            >
              {r} days
            </button>
          ))}
        </div>
      </div>

      <div className="chart-reading">
        <p className="chart-detail" aria-live="polite">
          {total > 0 ? (
            <>
              {focusLabel} · <strong className="num">{focusValue}</strong>
            </>
          ) : (
            "Your first dictation starts this chart."
          )}
        </p>
        {avg > 0 && <span className="chart-average-note num">Average {formatNumber(avg)} words · {active}/{days.length} days</span>}
      </div>

      <BarChart
        values={values}
        accent={last}
        tipLabel={(i) => (i === last ? `Today, ${shortDay(days[i].date)}` : longDay(days[i].date))}
        xTicks={ticks}
        reference={avg > 0 ? { value: avg } : null}
        height={146}
        ariaLabel={`Words per day for the last ${range} days: ${formatNumber(total)} in total, ${active} days used.`}
        emptyText="Your first dictation starts this chart."
        className="chart--daily"
        onActiveChange={setHovered}
      />
    </section>
  );
}

function HourlyCard({ s }: { s: StatsData }) {
  const values = s.hourly;
  const busiest = s.busiestHour;
  const total = values.reduce((a, b) => a + b, 0);
  const ticks = [0, 6, 12, 18].map((i) => ({ i, label: hourLabel(i) }));
  const [hovered, setHovered] = useState<number | null>(null);

  const focusIdx = hovered ?? busiest;
  const share = busiest != null && total > 0 ? Math.round((values[busiest] / total) * 100) : 0;

  return (
    <section className="chart-card" data-chart="hour" aria-labelledby="hourly-title">
      <div className="chart-card__head">
        <h2 id="hourly-title" className="chart-card__title">
          <Clock size={16} strokeWidth={2} aria-hidden="true" />
          Time of day
        </h2>
      </div>
      <div className="chart-reading">
        <p className="chart-detail" aria-live="polite">
          {focusIdx != null ? (
            <>
              {hourSpan(focusIdx)} · <strong className="num">{plural(values[focusIdx] ?? 0, "word", "words")}</strong>
              {hovered == null && share > 0 ? ` (${share}%)` : ""}
            </>
          ) : (
            "Appears after a few dictations."
          )}
        </p>
      </div>
      <BarChart
        values={values}
        accent={busiest}
        tipLabel={(i) => hourSpan(i)}
        xTicks={ticks}
        height={96}
        ariaLabel={busiest != null ? `Words by hour of day. Busiest hour: ${hourSpan(busiest)}.` : "Words by hour of day: none yet."}
        emptyText="Nothing yet."
        className="chart--hourly"
        onActiveChange={setHovered}
      />
    </section>
  );
}

function SpeedCard({ s, range, typingWpm }: { s: StatsData; range: 7 | 30; typingWpm: number }) {
  const all = s.wpmAll != null ? Math.round(s.wpmAll) : null;
  const factor = all != null && typingWpm > 0 ? all / typingWpm : null;
  const days = s.daily.slice(-range);
  const series = days.map((d) => dayWpm(d.words, d.speakingMs));
  const last = days.length - 1;
  const spoken = series.filter((v): v is number => v != null);
  const [hovered, setHovered] = useState<number | null>(null);

  const ticks =
    range === 7
      ? [0, 3, last].filter((v, idx, arr) => v >= 0 && v < days.length && arr.indexOf(v) === idx).map((i) => ({ i, label: i === last ? "Today" : weekdayShort(days[i].date) }))
      : [0, 7, 14, 21]
          .filter((i) => i < last - 3)
          .map((i) => ({ i, label: shortDay(days[i].date) }))
          .concat(last >= 0 ? [{ i: last, label: "Today" }] : []);

  const focusVal = hovered != null && series[hovered] != null ? Math.round(series[hovered]!) : all;
  const focusTitle =
    hovered != null && series[hovered] != null
      ? hovered === last
        ? `Today, ${shortDay(days[hovered].date)}`
        : longDay(days[hovered].date)
      : "Average";

  return (
    <section className="chart-card" data-chart="speed" aria-labelledby="speed-title">
      <div className="chart-card__head">
        <h2 id="speed-title" className="chart-card__title">
          <Gauge size={16} strokeWidth={2} aria-hidden="true" />
          Speaking speed
        </h2>
      </div>
      <div className="chart-reading">
        <p className="chart-detail" aria-live="polite">
          {focusVal != null ? (
            <>
              {focusTitle} · <strong className="num">{focusVal} wpm</strong>
              {hovered == null && factor != null && factor > 1.05 ? ` · ${factor.toFixed(1)}× typing` : ""}
            </>
          ) : (
            "Words ÷ minutes spoken, once you dictate."
          )}
        </p>
      </div>
      <LineChart
        values={series}
        accent={series[last] != null ? last : null}
        tipLabel={(i) => (i === last ? `Today, ${shortDay(days[i].date)}` : longDay(days[i].date))}
        tipValue={(i) => `${Math.round(series[i] ?? 0)} wpm`}
        xTicks={ticks}
        reference={{ value: typingWpm, label: `Typing ${typingWpm}` }}
        height={96}
        ariaLabel={
          spoken.length
            ? `Words per minute per day for the last ${range} days, from ${Math.round(Math.min(...spoken))} to ${Math.round(Math.max(...spoken))}; typing speed ${typingWpm}.`
            : "Words per minute per day: none yet."
        }
        emptyText="Nothing spoken yet."
        className="chart--speed"
        onActiveChange={setHovered}
      />
    </section>
  );
}

let lastStats: StatsData | null = null;

export default function Stats({ typingWpm: typingWpmSetting }: { typingWpm?: number }) {
  const constants = useConstants();
  const typingWpm = typingWpmSetting ?? constants.typingWpmDefault;
  const [s, setS] = useState<StatsData | null>(lastStats);
  const [range, setRange] = useState<7 | 30>(30);
  const [failed, setFailed] = useState(false);

  const load = useCallback(async () => {
    try {
      const next = await statsGet();
      lastStats = next;
      setS(next);
      setFailed(false);
    } catch {
      setFailed(true);
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
          <p className="page__lede">Your activity totals stay here after individual dictations are deleted.</p>
        </div>
      </header>

      {!s && failed && <LoadError what="your stats" onRetry={() => void load()} />}
      {s && (
        <>
          <SummaryCards s={s} range={range} typingWpm={typingWpm} />
          <DailyCard s={s} range={range} onRange={setRange} />
          <div className="stats-lower">
            <HourlyCard s={s} />
            <SpeedCard s={s} range={range} typingWpm={typingWpm} />
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
