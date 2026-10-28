/**
 * Loop arithmetic for AUTO BEAT LOOP, BEAT LOOP pads and manual loops.
 * Pure module: runs under `node --test`.
 */

export type BeatRange = { min: number; max: number };
/** What the current Mixxx host validates for `deck.loop.beats`. */
export const LOOP_BEATS_RANGE: BeatRange = { min: 1 / 8, max: 64 };
/** rekordbox range, used once the host declares the extended capability. */
export const LOOP_BEATS_RANGE_EXTENDED: BeatRange = { min: 1 / 64, max: 512 };

export type BeatGrid = { bpm?: number | null; beatgridOffsetMs?: number; beatTimesMs?: readonly number[] };
export type LoopSpan = { startMs: number; endMs: number };

/** "1/2", "4", "1/64". */
export function formatBeats(beats: number): string {
  if (!Number.isFinite(beats) || beats <= 0) return "—";
  if (beats >= 1) return String(Number(beats.toFixed(3)));
  const denominator = Math.round(1 / beats);
  return Math.abs(1 / denominator - beats) < 1e-6 ? `1/${denominator}` : String(Number(beats.toFixed(4)));
}

export const clampBeats = (beats: number, range: BeatRange) => Math.max(range.min, Math.min(range.max, beats));

/** Index of the last beat at or before `ms` (−1 when before the first beat). */
function beatIndexAt(times: readonly number[], ms: number): number {
  let low = 0, high = times.length;
  while (low < high) { const mid = (low + high) >> 1; if (times[mid] <= ms) low = mid + 1; else high = mid; }
  return low - 1;
}

/** Beat length near `ms`. Null when the track has no grid. */
export function beatLengthAt(grid: BeatGrid, ms: number): number | null {
  const times = grid.beatTimesMs;
  if (times && times.length >= 2) {
    const index = Math.max(0, Math.min(times.length - 2, beatIndexAt(times, ms)));
    const length = times[index + 1] - times[index];
    return length > 0 ? length : null;
  }
  return grid.bpm && grid.bpm > 0 ? 60_000 / grid.bpm : null;
}

/** Nearest beat to `ms` (quantize). Returns `ms` unchanged without a grid. */
export function snapToBeat(ms: number, grid: BeatGrid): number {
  const times = grid.beatTimesMs;
  if (times && times.length) {
    const index = beatIndexAt(times, ms);
    const before = times[Math.max(0, index)], after = times[Math.min(times.length - 1, index + 1)];
    return Math.abs(ms - before) <= Math.abs(after - ms) ? before : after;
  }
  const beat = grid.bpm && grid.bpm > 0 ? 60_000 / grid.bpm : 0;
  if (!beat) return ms;
  const offset = grid.beatgridOffsetMs ?? 0;
  return offset + Math.round((ms - offset) / beat) * beat;
}

/** Length of a loop in beats, or null without a grid. */
export function loopBeats(loop: LoopSpan, grid: BeatGrid): number | null {
  const beat = beatLengthAt(grid, loop.startMs);
  if (!beat) return null;
  return (loop.endMs - loop.startMs) / beat;
}

/** True when two beat counts describe the same loop length (2% tolerance). */
export const sameBeats = (a: number | null | undefined, b: number) => a != null && Math.abs(a - b) <= b * 0.02;

/**
 * Halve or double a loop while keeping its IN point, like rekordbox's
 * 1/2X / 2X. Returns null when the result would leave the track or the range.
 */
export function resizeLoop(loop: LoopSpan, factor: number, durationMs: number, grid: BeatGrid, range: BeatRange): LoopSpan | null {
  const length = (loop.endMs - loop.startMs) * factor;
  const beat = beatLengthAt(grid, loop.startMs);
  if (beat) {
    const beats = length / beat;
    if (beats < range.min * 0.999 || beats > range.max * 1.001) return null;
  } else if (length < 1) return null;
  const endMs = loop.startMs + length;
  if (endMs > durationMs) return null;
  return { startMs: loop.startMs, endMs };
}

/** Next AUTO BEAT LOOP size for ‹ / ›. */
export function stepBeats(beats: number, direction: -1 | 1, range: BeatRange): number {
  return clampBeats(direction < 0 ? beats / 2 : beats * 2, range);
}

export type BeatLoopPress = { kind: "create"; beats: number } | { kind: "exit" } | { kind: "resize"; loop: LoopSpan };

/**
 * BEAT LOOP pad / AUTO button semantics (rekordbox): pressing the size that is
 * already looping exits; another size while looping keeps the IN point; when
 * not looping a new loop starts at the playhead.
 */
export function beatLoopPress(active: LoopSpan | null, beats: number, grid: BeatGrid, durationMs: number, range: BeatRange): BeatLoopPress {
  if (!active) return { kind: "create", beats };
  const current = loopBeats(active, grid);
  if (sameBeats(current, beats) || current === null) return { kind: "exit" };
  const loop = resizeLoop(active, beats / current, durationMs, grid, range);
  return loop ? { kind: "resize", loop } : { kind: "exit" };
}

/** Manual loop OUT: needs a pending IN before the playhead. */
export function manualLoopOut(inMs: number | null, positionMs: number): LoopSpan | null {
  return inMs !== null && positionMs > inMs + 1 ? { startMs: inMs, endMs: positionMs } : null;
}
