// Spring physics as a CSS easing. A damped spring (stiffness k, damping c, mass 1) going from 0 to 1 is
// sampled into a CSS `linear()` easing, so size changes get a real spring's settle with plain CSS
// transitions (WebView2 / Chromium supports `linear()` since v113).

export interface SpringSpec {
  stiffness: number;
  damping: number;
}

/** Position of the spring at time t (seconds), starting at rest at 0 and settling at 1. */
export function springAt({ stiffness: k, damping: c }: SpringSpec, t: number): number {
  const w0 = Math.sqrt(k);
  const zeta = c / (2 * w0);
  if (zeta < 1) {
    const wd = w0 * Math.sqrt(1 - zeta * zeta);
    return 1 - Math.exp(-zeta * w0 * t) * (Math.cos(wd * t) + ((zeta * w0) / wd) * Math.sin(wd * t));
  }
  // critically damped (or over): use the critical form, close enough for UI
  return 1 - Math.exp(-w0 * t) * (1 + w0 * t);
}

/** Time (ms) until the spring stays within `eps` of 1. */
export function springDuration(spec: SpringSpec, eps = 0.002): number {
  let lastOutside = 0;
  for (let t = 0; t <= 3; t += 0.004) {
    if (Math.abs(1 - springAt(spec, t)) > eps) lastOutside = t;
  }
  return Math.round((lastOutside + 0.004) * 1000);
}

/** `{ easing: "linear(…)", ms }` for a CSS transition. */
export function springEasing(spec: SpringSpec, points = 48): { easing: string; ms: number } {
  const ms = springDuration(spec);
  const stops: string[] = [];
  for (let i = 0; i <= points; i++) {
    const p = i / points;
    const v = i === points ? 1 : springAt(spec, (p * ms) / 1000);
    stops.push(v.toFixed(4).replace(/\.?0+$/, "") || "0");
  }
  return { easing: `linear(${stops.join(", ")})`, ms };
}

export function supportsLinearEasing(): boolean {
  return typeof CSS !== "undefined" && CSS.supports("transition-timing-function", "linear(0, 0.5, 1)");
}

/** Pill grow/shrink: ζ ≈ 0.72, about 4 % overshoot — lively but calm. */
export const POPUP_SPRING: SpringSpec = { stiffness: 380, damping: 28 };
