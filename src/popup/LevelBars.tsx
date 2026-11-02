// The level meter: 11 bars driven by the real mic level, rippling out from the centre.
//
// Maths (per animation frame, dt capped at 50 ms; see task-6-report.md for why each number):
//  1. raw = latest "popup://level" value (0..1, ~30 Hz, already perceptual). If no event for 250 ms,
//     raw decays to 0, so a stalled stream never freezes the bars mid-air.
//  2. Gate + curve: target = raw < 0.04 ? 0 : ((raw - 0.04) / 0.96) ^ 1.15   (noise gate, Glaido/Willow)
//  3. Envelope: e += (target - e) * (1 - exp(-dt / tau)), tau = 35 ms rising, 170 ms falling
//     (fast attack, slow release, frame-rate independent).
//  4. Ripple: the envelope is recorded with timestamps; bar i at distance d = |i - 5| / 5 from the centre
//     reads the envelope as it was d × 120 ms ago and scales it by (1 - 0.45 d). The centre leads, the
//     edges echo it — a soft ripple outwards (Willow's idea, time-based instead of frame-based).
//  5. Life: × (0.84 + 0.16 · (0.5 + 0.5 sin(2π · 1.1 t + 1.9 i))) so neighbours never move as one block.
//  6. Idle breath (no reduced motion): floor = 0.07 · (0.5 + 0.5 sin(2π · 0.55 t − 2.6 d)), a slow wave
//     travelling outwards while silent.
//  7. Each bar follows its target through a spring (k = 560, ζ = 0.8; ζ = 1 with reduced motion), which
//     smooths the 30 Hz input into 60 fps motion with a hint of bounce.
//  8. Height = 3 + 15 v px (3..18), opacity = 0.62 + 0.38 v.
import { useEffect, useRef } from "react";

export const BAR_COUNT = 11;
const BAR_MIN = 3;
const BAR_MAX = 18;

/** Shared latest level, written by the event listener (or the mock generator). */
export const levelBus = {
  value: 0,
  at: 0,
  heard: false,
  push(v: number) {
    this.value = Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 0;
    this.at = performance.now();
    this.heard = true;
  },
  reset() {
    this.value = 0;
    this.at = 0;
    this.heard = false;
  },
};

export type BarsMode = "live" | "waiting" | "frozen";

interface Props {
  mode: BarsMode;
  reducedMotion: boolean;
}

export function LevelBars({ mode, reducedMotion }: Props) {
  const barsRef = useRef<Array<HTMLSpanElement | null>>([]);
  const modeRef = useRef(mode);
  modeRef.current = mode;

  useEffect(() => {
    let raf = 0;
    let last = performance.now();
    let env = 0;
    const hist: Array<{ t: number; e: number }> = [];
    const pos = new Array(BAR_COUNT).fill(0);
    const vel = new Array(BAR_COUNT).fill(0);
    const k = 560;
    const zeta = reducedMotion ? 1 : 0.8;
    const c = 2 * zeta * Math.sqrt(k);

    const envAt = (t: number) => {
      // hist is oldest first: scan back from the newest to the last sample at or before t, then
      // interpolate towards the next newer sample
      for (let j = hist.length - 1; j >= 0; j--) {
        if (hist[j].t <= t) {
          const a = hist[j];
          const b = hist[j + 1];
          if (!b) return a.e;
          const f = (t - a.t) / Math.max(1, b.t - a.t);
          return a.e + (b.e - a.e) * f;
        }
      }
      return hist.length ? hist[0].e : 0;
    };

    const frame = (now: number) => {
      const dt = Math.min(0.05, (now - last) / 1000);
      last = now;
      const m = modeRef.current;
      const t = now / 1000;

      let raw = 0;
      if (m === "live") {
        raw = levelBus.value;
        const age = now - levelBus.at;
        if (age > 250) raw *= Math.max(0, 1 - (age - 250) / 250);
      }
      const target = raw < 0.04 ? 0 : Math.pow((raw - 0.04) / 0.96, 1.15);
      const tau = target > env ? 0.035 : 0.17;
      env += (target - env) * (1 - Math.exp(-dt / tau));

      hist.push({ t: now, e: env });
      while (hist.length > 2 && hist[1].t < now - 300) hist.shift();

      for (let i = 0; i < BAR_COUNT; i++) {
        const d = Math.abs(i - (BAR_COUNT - 1) / 2) / ((BAR_COUNT - 1) / 2);
        let goal = 0;
        if (m === "live") {
          const delayed = reducedMotion ? env : envAt(now - d * 120);
          const life = reducedMotion ? 1 : 0.84 + 0.16 * (0.5 + 0.5 * Math.sin(2 * Math.PI * 1.1 * t + 1.9 * i));
          goal = delayed * (1 - 0.45 * d) * life;
        }
        if (!reducedMotion && m !== "frozen") {
          const breath = (m === "waiting" ? 0.12 : 0.07) * (0.5 + 0.5 * Math.sin(2 * Math.PI * 0.55 * t - 2.6 * d));
          goal = Math.max(goal, breath);
        }
        // spring step (semi-implicit Euler, two substeps for stability)
        const h = dt / 2;
        for (let s = 0; s < 2; s++) {
          const a = k * (goal - pos[i]) - c * vel[i];
          vel[i] += a * h;
          pos[i] += vel[i] * h;
        }
        const v = Math.min(1.08, Math.max(0, pos[i]));
        const el = barsRef.current[i];
        if (el) {
          el.style.height = `${(BAR_MIN + (BAR_MAX - BAR_MIN) * Math.min(1, v)).toFixed(2)}px`;
          el.style.opacity = m === "live" ? (0.62 + 0.38 * Math.min(1, v)).toFixed(3) : "";
        }
      }
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);
    return () => cancelAnimationFrame(raf);
  }, [reducedMotion]);

  return (
    <span className={`bars bars--${mode}`} aria-hidden="true">
      {Array.from({ length: BAR_COUNT }, (_, i) => (
        <span key={i} className="bars__bar" ref={(el) => void (barsRef.current[i] = el)} style={{ ["--i" as string]: i }} />
      ))}
    </span>
  );
}
