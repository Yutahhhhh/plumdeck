/** ±percent the DJ allows a pick to drift from the deck's tempo; null keeps each preset's own window. */
export type TempoTolerance = number | null;

export const TEMPO_TOLERANCES: number[] = [1, 2, 3, 4, 6];
export const TEMPO_TOLERANCE_STORAGE_KEY = "plumdeck.assist.tempoTolerance";

export function parseTempoTolerance(raw: string | null): TempoTolerance {
  if (raw === null) return null;
  const value = Number(raw);
  return TEMPO_TOLERANCES.includes(value) ? value : null;
}

/** The DJ's tempo habit is shared by Assist and Play RECOMMEND and remembered across sessions. */
export function loadTempoTolerance(): TempoTolerance {
  try { return parseTempoTolerance(localStorage.getItem(TEMPO_TOLERANCE_STORAGE_KEY)); }
  catch { return null; }
}

export function saveTempoTolerance(value: TempoTolerance) {
  try {
    if (value === null) localStorage.removeItem(TEMPO_TOLERANCE_STORAGE_KEY);
    else localStorage.setItem(TEMPO_TOLERANCE_STORAGE_KEY, String(value));
  } catch { /* the setting still applies for this window */ }
}
