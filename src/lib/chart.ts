// Pure chart maths (no imports; unit-tested by scripts/unit-tests.mjs).

/**
 * Friendly axis ticks from 0 to just above `max`, about `target` steps, always whole numbers:
 * 1 → [0, 1], 7 → [0, 5, 10], 1,480 → [0, 500, 1000, 1500].
 */
export function niceTicks(max: number, target = 3): number[] {
  if (!(max > 0)) return [0];
  const rough = max / target;
  const pow = 10 ** Math.floor(Math.log10(rough));
  const mults = pow < 10 ? [1, 2, 5, 10] : [1, 2, 2.5, 5, 10]; // 2.5 only where it stays whole
  const step = Math.max(1, mults.map((m) => m * pow).find((s) => s >= rough) ?? 10 * pow);
  const top = Math.ceil(max / step) * step;
  const out: number[] = [];
  for (let k = 0; k * step <= top; k++) out.push(Math.round(k * step));
  return out;
}

/** Words per minute for one day, or null when nothing was spoken (drawn as a gap, not a zero). */
export function dayWpm(words: number, speakingMs: number | undefined): number | null {
  if (!speakingMs || speakingMs <= 0 || words <= 0) return null;
  return (words * 60_000) / speakingMs;
}
