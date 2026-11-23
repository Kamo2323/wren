// Single-series line chart (the speaking speed words-per-minute trend) with teal theme accent
// and dashed typing baseline reference line.
import { useId, useLayoutEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { niceTicks } from "../lib/chart";

export interface LineChartProps {
  values: Array<number | null>;
  accent?: number | null;
  tipLabel: (i: number) => string;
  tipValue: (i: number) => string;
  xTicks: Array<{ i: number; label: string }>;
  reference?: { value: number; label: string } | null;
  height?: number;
  ariaLabel: string;
  emptyText?: ReactNode;
  className?: string;
  onActiveChange?: (index: number | null) => void;
}

const PAD_X = 7;

export function LineChart({
  values,
  accent,
  tipLabel,
  tipValue,
  xTicks,
  reference,
  height = 128,
  ariaLabel,
  emptyText,
  className = "",
  onActiveChange,
}: LineChartProps) {
  const gradId = useId();
  const plotRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);
  const [hover, setHover] = useState<number | null>(null);

  useLayoutEffect(() => {
    const el = plotRef.current;
    if (!el) return;
    setWidth(el.clientWidth);
    const ro = new ResizeObserver(() => setWidth(el.clientWidth));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const n = values.length;
  const present = values.filter((v): v is number => v != null);
  const empty = present.length === 0;
  const ticks = niceTicks(Math.max(0, ...present, reference?.value ?? 0));
  const top = ticks[ticks.length - 1] || 1;
  const x = (i: number) => (n <= 1 ? width / 2 : PAD_X + (i / (n - 1)) * (width - 2 * PAD_X));
  const y = (v: number) => height - (v / top) * height;

  const runs: number[][] = [];
  values.forEach((v, i) => {
    if (v == null) return;
    const run = runs[runs.length - 1];
    if (run && run[run.length - 1] === i - 1) run.push(i);
    else runs.push([i]);
  });
  const line = (run: number[]) => run.map((i, k) => `${k ? "L" : "M"}${x(i).toFixed(1)},${y(values[i]!).toFixed(1)}`).join("");
  const area = (run: number[]) => `${line(run)}L${x(run[run.length - 1]).toFixed(1)},${height}L${x(run[0]).toFixed(1)},${height}Z`;

  function setHovered(i: number | null) {
    setHover(i);
    onActiveChange?.(i);
  }

  function onMove(e: MouseEvent<HTMLDivElement>) {
    if (empty || width === 0) return;
    const r = e.currentTarget.getBoundingClientRect();
    const px = e.clientX - r.left;
    let best: number | null = null;
    let dist = Infinity;
    values.forEach((v, i) => {
      if (v == null) return;
      const d = Math.abs(x(i) - px);
      if (d < dist) {
        dist = d;
        best = i;
      }
    });
    setHovered(best);
  }

  const h = hover != null && values[hover] != null ? hover : null;
  const tipEdge = h == null ? "" : x(h) < 70 ? "is-start" : x(h) > width - 70 ? "is-end" : "";

  return (
    <div className={`chart chart--line ${empty ? "is-empty" : ""} ${className}`} style={{ ["--h" as string]: `${height}px` }}>
      <div className="chart__y" aria-hidden="true">
        {!empty &&
          ticks.map((t) => (
            <span key={t} className="chart__ytick num" style={{ bottom: `${(t / top) * 100}%` }}>
              {t}
            </span>
          ))}
      </div>

      <div ref={plotRef} className="chart__plot" role="img" aria-label={ariaLabel} onMouseMove={onMove} onMouseLeave={() => setHovered(null)}>
        {(empty ? [0] : ticks).map((t) => (
          <div key={t} className={`chart__grid ${t === 0 ? "is-base" : ""}`} style={{ bottom: `${(t / top) * 100}%` }} />
        ))}
        {reference && !empty && (
          <div className="chart__ref chart__ref--right" style={{ bottom: `${(reference.value / top) * 100}%` }}>
            <span className="chart__ref-label num">{reference.label}</span>
          </div>
        )}

        {width > 0 && !empty && (
          <svg className="line" width={width} height={height} aria-hidden="true">
            <defs>
              <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
                <stop offset="0" stopColor="var(--chart-active, var(--teal-deep))" stopOpacity="0.24" />
                <stop offset="1" stopColor="var(--chart-active, var(--teal-deep))" stopOpacity="0" />
              </linearGradient>
            </defs>
            {runs.map((run) => (
              <path key={`a${run[0]}`} className="line__area" d={area(run)} fill={`url(#${gradId})`} />
            ))}
            {runs.map((run) => (
              <path key={`l${run[0]}`} className="line__path" d={line(run)} />
            ))}
            {h != null && <line className="line__cross" x1={x(h)} x2={x(h)} y1={0} y2={height} />}
            {values.map((v, i) =>
              v == null ? null : (
                <circle
                  key={i}
                  className={`line__dot ${i === accent ? "is-accent" : ""} ${i === h ? "is-hover" : ""}`}
                  cx={x(i)}
                  cy={y(v)}
                  r={i === accent || i === h ? 4.8 : 2.6}
                />
              ),
            )}
          </svg>
        )}

        {h != null && (
          <div
            className={`chart__tip ${tipEdge}`}
            style={{
              left: tipEdge === "is-end" ? undefined : tipEdge === "is-start" ? Math.max(0, x(h) - 14) : x(h),
              right: tipEdge === "is-end" ? Math.max(0, width - x(h) - 14) : undefined,
              bottom: height - y(values[h]!) + 12,
            }}
            role="presentation"
          >
            <span className="chart__tip-label">{tipLabel(h)}</span>
            <span className="chart__tip-value num">{tipValue(h)}</span>
          </div>
        )}

        {empty && emptyText && <div className="chart__empty">{emptyText}</div>}
      </div>

      <div className="chart__spacer" aria-hidden="true" />
      <div className="chart__x chart__x--abs" aria-hidden="true">
        {width > 0 &&
          xTicks.map((t) => (
            <span key={t.i} className={`chart__xtick ${t.i === 0 ? "is-first" : t.i === n - 1 ? "is-last" : ""}`} style={{ left: x(t.i) }}>
              {t.label}
            </span>
          ))}
      </div>

      <table className="sr-only">
        <caption>{ariaLabel}</caption>
        <tbody>
          {values.map((v, i) => (
            <tr key={i}>
              <th scope="row">{tipLabel(i)}</th>
              <td>{v == null ? "nothing spoken" : tipValue(i)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
