// Home's calm waveform hero card: soft blue-to-violet gradient, drawn wave canvas, and live
// Time saved, Speaking speed and Current streak metrics.
import { useEffect, useRef } from "react";
import type { Stats } from "../lib/types";
import type { ResolvedTheme } from "../lib/theme";
import { computeTimeSavedMs, formatSaved, formatSavedShort } from "../lib/format";
import { Clock, Flame, Gauge } from "./Icon";

function drawWave(canvas: HTMLCanvasElement, light: boolean) {
  const { width, height } = canvas.getBoundingClientRect();
  if (!width || !height) return;
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  canvas.width = Math.ceil(width * dpr);
  canvas.height = Math.ceil(height * dpr);
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, width, height);

  const inks = light ? ["113, 85, 178", "51, 132, 146"] : ["193, 167, 255", "130, 216, 206"];
  const midY = height * 0.54;
  const amplitude = Math.min(height * 0.3, width * 0.18);
  const envelope = (t: number) => Math.pow(Math.sin(t * Math.PI), 1.4) * (0.85 + t * 0.4);
  const wave = (t: number, lane: number, band: number) =>
    midY +
    envelope(t) *
      amplitude *
      (Math.sin(t * 12.8 - 1.8 + band * 1.1) * (0.58 + lane * 0.035) +
        Math.sin(t * 19 - lane * 0.095 + band * 0.9) * 0.21 +
        (lane - 4.5) * 0.11);

  for (let band = 0; band < inks.length; band++) {
    for (let lane = 0; lane < 10; lane++) {
      const fade = ctx.createLinearGradient(0, 0, width, 0);
      fade.addColorStop(0, `rgba(${inks[band]}, 0)`);
      fade.addColorStop(0.42, `rgba(${inks[band]}, ${light ? 0.24 : 0.25})`);
      fade.addColorStop(0.74, `rgba(${inks[band]}, ${light ? 0.4 : 0.48})`);
      fade.addColorStop(1, `rgba(${inks[band]}, 0.035)`);
      ctx.strokeStyle = fade;
      ctx.lineWidth = lane === 7 ? 1.15 : 0.67;
      ctx.beginPath();
      for (let step = 0; step <= 155; step++) {
        const t = step / 155;
        const x = width * t;
        const y = wave(t, lane, band);
        if (step === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
    }
  }

  let seed = 1637;
  const random = () => {
    seed = (seed * 16807) % 2147483647;
    return seed / 2147483647;
  };
  for (let dot = 0; dot < 270; dot++) {
    const t = 0.11 + random() * 0.87;
    const band = dot % 2;
    const y = wave(t, random() * 9, band) + (random() - 0.5) * 28;
    ctx.fillStyle = `rgba(${inks[band]}, ${envelope(t) * random() * 0.26})`;
    ctx.fillRect(t * width, y, 1, 1);
  }
}

export function WaveHero({
  stats,
  typingWpm,
  theme,
}: {
  stats: Stats | null;
  typingWpm: number;
  theme: ResolvedTheme;
}) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const paint = () => drawWave(canvas, theme === "light");
    paint();
    const ro = new ResizeObserver(paint);
    ro.observe(canvas);
    return () => ro.disconnect();
  }, [theme]);

  if (!stats) {
    return (
      <section className="hero hero--loading stats--loading" aria-hidden="true">
        <canvas ref={canvasRef} className="hero__canvas" aria-hidden="true" />
      </section>
    );
  }

  const last7 = stats.daily.slice(-7);
  const words7 = last7.reduce((sum, d) => sum + d.words, 0);
  const speak7 = last7.reduce((sum, d) => sum + d.speakingMs, 0);
  const saved7Ms = computeTimeSavedMs(words7, speak7, typingWpm);

  const empty = stats.dictationsAll === 0;
  const useAllTime = !empty && words7 === 0 && stats.timeSavedMsAll > 0;
  const savedMs = useAllTime ? stats.timeSavedMsAll : saved7Ms;
  const savedParts = formatSaved(savedMs);
  const savedPeriod = empty
    ? "Starts when you dictate"
    : useAllTime
      ? "All time"
      : stats.timeSavedMsAll > saved7Ms
        ? `Last 7 days · ${formatSavedShort(stats.timeSavedMsAll)} total`
        : "Last 7 days";

  const wpm = stats.wpmAll != null ? Math.round(stats.wpmAll) : null;
  const factor = wpm != null && typingWpm > 0 ? wpm / typingWpm : null;
  const speedPeriod =
    factor != null && factor > 1.05
      ? `${factor.toFixed(1)}× typing speed`
      : stats.wpmToday != null
        ? `${Math.round(stats.wpmToday)} wpm today`
        : "After your first dictation";

  const streakPeriod = stats.streakDays > 0 ? "Keep it going" : "Start one today";

  return (
    <section className="hero" aria-labelledby="activity-title">
      <canvas ref={canvasRef} className="hero__canvas" aria-hidden="true" />
      <h2 id="activity-title" className="hero__title">
        Dictation activity
      </h2>
      <div className="hero-metrics">
        <div
          className={`stat activity-metric ${empty ? "is-muted" : ""}`}
          data-tone="violet"
          title={`Words ÷ your typing speed (${typingWpm} wpm), minus speaking time.`}
        >
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
          <span className="hero-metric-period">{savedPeriod}</span>
        </div>

        <div
          className={`stat activity-metric ${wpm == null ? "is-muted" : ""}`}
          data-tone="teal"
          title="Words ÷ minutes spent speaking, across all dictations."
        >
          <span className="metric-label">
            <Gauge size={15} strokeWidth={2} aria-hidden="true" />
            Speaking speed
          </span>
          <strong className="metric num">
            {wpm != null ? wpm : "–"}
            {wpm != null && <small>wpm</small>}
          </strong>
          <span className="hero-metric-period">{speedPeriod}</span>
        </div>

        <div
          className={`stat activity-metric ${stats.streakDays === 0 ? "is-muted" : ""}`}
          data-tone="amber"
          title="Days in a row with at least one dictation, ending today or yesterday."
        >
          <span className="metric-label">
            <Flame size={15} strokeWidth={2} aria-hidden="true" />
            Current streak
          </span>
          <strong className="metric num">
            {stats.streakDays}
            <small>{stats.streakDays === 1 ? "day" : "days"}</small>
          </strong>
          <span className="hero-metric-period">{streakPeriod}</span>
        </div>
      </div>
    </section>
  );
}
