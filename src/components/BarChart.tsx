// Single-series column chart for the Stats page (one hue, one highlighted bar, recessive grid).
// Bars are CSS grid columns so the x labels share the same columns; hover shows a tooltip above the bar.
import { useState, type ReactNode } from "react";
import { formatNumber } from "../lib/format";
import { niceTicks } from "../lib/chart";

export interface BarChartProps {
  values: number[];
  /** Index drawn in the full accent (today, or the busiest hour). */
  accent?: number | null;
  /** Tooltip heading for bar i, e.g. "Tue 28 Oct" or "3 pm – 4 pm". */
  tipLabel: (i: number) => string;
  /** Tooltip value line for bar i. Defaults to "1,234 words". */
  tipValue?: (i: number) => string;
  /** Labels under the bars: bar index → text. */
  xTicks: Array<{ i: number; label: string }>;
  /** Draw a dashed reference line (e.g. daily average) with a short label. */
  reference?: { value: number; label: string } | null;
  height?: number;
  ariaLabel: string;
  /** Shown over an all-zero chart. */
  emptyText?: ReactNode;
  className?: string;
}


function compact(n: number): string {
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  if (n >= 1000) return `${(n / 1000).toFixed(n % 1000 === 0 ? 0 : 1)}k`;
  return String(n);
}

export function BarChart({ values, accent, tipLabel, tipValue, xTicks, reference, height = 220, ariaLabel, emptyText, className = "" }: BarChartProps) {
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(0, ...values);
  const ticks = niceTicks(max);
  const top = ticks[ticks.length - 1] || 1;
  const n = values.length;
  const empty = max === 0;
  const value = tipValue ?? ((i: number) => `${formatNumber(values[i])} ${values[i] === 1 ? "word" : "words"}`);

  return (
    <div className={`chart ${empty ? "is-empty" : ""} ${className}`} style={{ ["--n" as string]: n, ["--h" as string]: `${height}px` }}>
      <div className="chart__y" aria-hidden="true">
        {!empty &&
          ticks.map((t) => (
            <span key={t} className="chart__ytick num" style={{ bottom: `${(t / top) * 100}%` }}>
              {compact(t)}
            </span>
          ))}
      </div>

      <div className="chart__plot" role="img" aria-label={ariaLabel} onMouseLeave={() => setHover(null)}>
        {(empty ? [0] : ticks).map((t) => (
          <div key={t} className={`chart__grid ${t === 0 ? "is-base" : ""}`} style={{ bottom: `${(t / top) * 100}%` }} />
        ))}

        {reference && !empty && reference.value > 0 && (
          <div className="chart__ref" style={{ bottom: `${(reference.value / top) * 100}%` }}>
            <span className="chart__ref-label num">{reference.label}</span>
          </div>
        )}

        <div className="chart__bars">
          {values.map((v, i) => {
            const pct = v > 0 ? Math.max(1.5, (v / top) * 100) : 0;
            const edge = i < 2 ? "is-start" : i > n - 3 ? "is-end" : "";
            return (
              <div
                key={i}
                className={`chart__col ${i === accent ? "is-accent" : ""} ${hover === i ? "is-hover" : ""} ${v === 0 ? "is-zero" : ""}`}
                onMouseEnter={() => setHover(i)}
              >
                <div className="chart__bar" style={{ height: v > 0 ? `${pct}%` : undefined, ["--i" as string]: i }} />
                {hover === i && (
                  <div className={`chart__tip ${edge}`} style={{ bottom: `calc(${pct}% + 10px)` }} role="presentation">
                    <span className="chart__tip-label">{tipLabel(i)}</span>
                    <span className="chart__tip-value num">{value(i)}</span>
                  </div>
                )}
              </div>
            );
          })}
        </div>

        {empty && emptyText && <div className="chart__empty">{emptyText}</div>}
      </div>

      <div className="chart__spacer" aria-hidden="true" />
      <div className="chart__x" aria-hidden="true">
        {xTicks.map((t) => (
          <span key={t.i} className="chart__xtick" style={{ gridColumn: t.i + 1 }}>
            {t.label}
          </span>
        ))}
      </div>

      <table className="sr-only">
        <caption>{ariaLabel}</caption>
        <tbody>
          {values.map((_, i) => (
            <tr key={i}>
              <th scope="row">{tipLabel(i)}</th>
              <td>{value(i)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
