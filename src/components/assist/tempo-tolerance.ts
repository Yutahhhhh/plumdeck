/** ±percent the DJ allows a pick to drift from the deck's tempo; null keeps each preset's own window. */
export type TempoTolerance = number | null;

export const TEMPO_TOLERANCES: number[] = [1, 2, 3, 4, 6];
export const TEMPO_TOLERANCE_STORAGE_KEY = "plumdeck.assist.tempoTolerance";

export function parseTempoTolerance(raw: string | null): TempoTolerance {
  if (raw === null) return null;
  const value = Number(raw);
  return TEMPO_TOLERANCES.includes(value) ? value : null;
}
