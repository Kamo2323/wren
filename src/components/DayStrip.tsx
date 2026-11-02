// The slim 30-day strip under the stat tiles: one bar per day, today in the accent colour.
import { useState } from "react";
import type { DayPoint } from "../lib/types";
import { formatNumber } from "../lib/format";

function label(date: string): string {
  const [y, m, d] = date.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString("en-GB", { weekday: "short", day: "numeric", month: "short" });
}

/** Holds the strip's exact height while the stats load, so the "Recent" heading never moves. */
export function DayStripPlaceholder() {
  return (
    <div className="strip is-placeholder" aria-hidden="true">
      <div className="strip__head">&nbsp;</div>
      <div className="strip__bars" />
      <div className="strip__axis">&nbsp;</div>
    </div>
  );
}

export function DayStrip({ days }: { days: DayPoint[] }) {
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(1, ...days.map((d) => d.words));
  const total = days.reduce((a, d) => a + d.words, 0);
  const last = days.length - 1;
  const h = hover !== null ? days[hover] : null;

  return (
    <div className="strip" onMouseLeave={() => setHover(null)}>
      <div className="strip__head">
        <span className="strip__title">Last 30 days</span>
        <span className="strip__value num">
          {h ? (
            <>
              <strong>{formatNumber(h.words)}</strong> {h.words === 1 ? "word" : "words"} · {hover === last ? "Today" : label(h.date)}
            </>
          ) : (
            <>
              <strong>{formatNumber(total)}</strong> words
            </>
          )}
        </span>
      </div>
      <div className="strip__bars" role="img" aria-label={`Words per day for the last 30 days, ${formatNumber(total)} in total`}>
        {days.map((d, i) => {
          const pct = d.words > 0 ? Math.max(8, (d.words / max) * 100) : 0;
          return (
            <div
              key={d.date}
              className={`strip__col ${i === last ? "is-today" : ""} ${hover === i ? "is-hover" : ""} ${d.words === 0 ? "is-zero" : ""}`}
              onMouseEnter={() => setHover(i)}
            >
              <div
                className="strip__bar"
                style={{ height: d.words > 0 ? `${pct}%` : undefined, ["--i" as string]: i }}
              />
            </div>
          );
        })}
      </div>
      <div className="strip__axis" aria-hidden="true">
        Today
      </div>
    </div>
  );
}
