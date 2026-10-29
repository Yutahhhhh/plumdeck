import { useSyncExternalStore } from "react";

/**
 * One zoom for every zoomed deck lane (rekordbox's common zoom). The visible
 * span is in real seconds; lanes multiply it by their deck's tempo so synced
 * decks show their beats at the same spacing.
 */
export const ZOOM_LEVELS = [0.01, 0.02, 0.05, 0.1, 0.25, 0.5, 1, 2, 4, 8, 16, 32] as const;
export const ZOOM_DEFAULT = 8;
const KEY = "plumdeck.waveformZoom";

function read(): number {
  try { const value = Number(localStorage.getItem(KEY)); return (ZOOM_LEVELS as readonly number[]).includes(value) ? value : ZOOM_DEFAULT; } catch { return ZOOM_DEFAULT; }
}
let current = typeof localStorage === "undefined" ? ZOOM_DEFAULT : read();
const listeners = new Set<() => void>();

export function setWaveformZoom(seconds: number) {
  const value = (ZOOM_LEVELS as readonly number[]).includes(seconds) ? seconds : ZOOM_DEFAULT;
  if (value === current) return;
  current = value;
  try { localStorage.setItem(KEY, String(value)); } catch { /* in-memory only */ }
  for (const listener of listeners) listener();
}
/** +1 zooms in (shorter span), −1 zooms out. */
export function stepWaveformZoom(direction: 1 | -1) {
  const index = (ZOOM_LEVELS as readonly number[]).indexOf(current);
  setWaveformZoom(ZOOM_LEVELS[Math.max(0, Math.min(ZOOM_LEVELS.length - 1, index - direction))]);
}
export const resetWaveformZoom = () => setWaveformZoom(ZOOM_DEFAULT);
export const getWaveformZoom = () => current;
const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
export function useWaveformZoom() { return useSyncExternalStore(subscribe, getWaveformZoom); }
/** Track-time span of a lane: real seconds × tempo. */
export function laneSpanMs(zoomSeconds: number, rate: number) { return zoomSeconds * 1000 * (Number.isFinite(rate) && rate > 0 ? rate : 1); }
