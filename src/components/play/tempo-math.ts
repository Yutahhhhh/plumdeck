/**
 * Tempo range, step and BPM formatting. Pure module: runs under `node --test`.
 */

/** ±6 / ±10 / ±16 / WIDE. WIDE is ±75% on the Mixxx host (rekordbox: ±100%). */
export const TEMPO_RANGES = [6, 10, 16, 75] as const;
export type TempoRange = (typeof TEMPO_RANGES)[number];
export const rangeLabel = (range: TempoRange) => range === 75 ? "WIDE" : `±${range}`;

/** rekordbox's ± button resolution per range, in percent. */
export function tempoStep(range: TempoRange): number {
  return range === 6 ? 0.02 : range === 75 ? 0.5 : 0.04;
}

/** Decimals the tempo readout needs so one step is visible. */
export const tempoDecimals = (range: TempoRange) => range === 75 ? 1 : 2;

/** Rate after moving the tempo by `steps` buttons, clamped to the range. */
export function stepTempo(rate: number, steps: number, range: TempoRange): number {
  const step = tempoStep(range);
  const percent = Math.round(((rate - 1) * 100) / step) * step + steps * step;
  return 1 + Math.max(-range, Math.min(range, Number(percent.toFixed(4)))) / 100;
}

export function clampRate(rate: number, range: TempoRange): number {
  return Math.max(1 - range / 100, Math.min(1 + range / 100, rate));
}

/** "+1.20%" / "−0.04%" with the range's resolution. */
export function formatTempoPercent(rate: number, range: TempoRange): string {
  const percent = (rate - 1) * 100;
  const text = Math.abs(percent).toFixed(tempoDecimals(range));
  return `${Number(text) === 0 ? "±" : percent > 0 ? "+" : "−"}${text}%`;
}

/**
 * BPM split into whole and two-digit fraction from one rounding, so
 * 127.996 reads 128.00 (never "127.100").
 */
export function formatBpm(bpm: number | null | undefined): { whole: string; fraction: string } | null {
  if (bpm === null || bpm === undefined || !Number.isFinite(bpm) || bpm <= 0) return null;
  const [whole, fraction] = bpm.toFixed(2).split(".");
  return { whole, fraction };
}
