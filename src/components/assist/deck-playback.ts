import { observedDeckIdentity } from "./deck-resolution-cache.ts";

/** rekordbox exposes no play state; a deck counts as playing while its elapsed readout advances. */
export interface PlaybackDeck { slot: number; loaded: boolean; title: string | null; artist: string | null; elapsed_seconds: number | null }

interface SlotPlayback {
  identity: string;
  /** Baseline of the last comparison; unchanged while the readout is unreadable. */
  elapsed: number | null;
  atMs: number;
  playing: boolean;
  /** When this run of playing was first observed; orders decks that play together. */
  since: number | null;
  idle: number;
}

export interface PlaybackState { atMs: number; slots: ReadonlyMap<number, SlotPlayback> }

export const EMPTY_PLAYBACK: PlaybackState = { atMs: Number.NEGATIVE_INFINITY, slots: new Map() };

/** Snapshots closer than this say nothing about motion (cached reads, manual refreshes). */
const MIN_SAMPLE_GAP_MS = 400;
/** Readout resolution is 0.1s; anything smaller is rounding, not motion. */
const ADVANCE_EPSILON = 0.1;
/** Consecutive samples without motion before a deck counts as stopped. */
export const STOP_AFTER_IDLE = 2;
/** A forward step beyond this multiple of the wall clock is a seek, not playback. */
const MAX_PLAY_RATE = 2;
const SEEK_SLACK_SECONDS = 0.5;

export function advancePlayback(prev: PlaybackState, atMs: number, decks: readonly PlaybackDeck[]): PlaybackState {
  if (!decks.length) return prev.slots.size ? EMPTY_PLAYBACK : prev;
  if (prev.slots.size && atMs - prev.atMs < MIN_SAMPLE_GAP_MS) return prev;
  const slots = new Map<number, SlotPlayback>();
  for (const deck of decks) {
    if (!deck.loaded) continue;
    const identity = observedDeckIdentity([deck]);
    const before = prev.slots.get(deck.slot);
    const elapsed = deck.elapsed_seconds;
    if (!before || before.identity !== identity || before.elapsed === null) {
      slots.set(deck.slot, { identity, elapsed, atMs, playing: false, since: null, idle: 0 });
      continue;
    }
    if (elapsed === null) {
      // A missed read is not motion, but a few of them must not flap the state.
      const idle = before.idle + 1;
      slots.set(deck.slot, { ...before, idle, playing: before.playing && idle < STOP_AFTER_IDLE, since: before.playing && idle < STOP_AFTER_IDLE ? before.since : null });
      continue;
    }
    const delta = elapsed - before.elapsed;
    const wallSeconds = (atMs - before.atMs) / 1000;
    let { playing, since, idle } = before;
    if (delta > ADVANCE_EPSILON && delta <= wallSeconds * MAX_PLAY_RATE + SEEK_SLACK_SECONDS) {
      since = playing ? since : atMs;
      playing = true;
      idle = 0;
    } else if (delta > ADVANCE_EPSILON) {
      // A forward seek says nothing about whether the deck is running.
    } else {
      // Unchanged, or jumped back (cue return, loop wrap): evidence of standing still.
      idle += 1;
      if (idle >= STOP_AFTER_IDLE) { playing = false; since = null; }
    }
    slots.set(deck.slot, { identity, elapsed, atMs, playing, since, idle });
  }
  return { atMs, slots };
}

export function playingSlots(state: PlaybackState): number[] {
  return [...state.slots].filter(([, slot]) => slot.playing).map(([slot]) => slot).sort((a, b) => a - b);
}

/** True when at least one loaded deck exposes an elapsed readout. */
export function playbackReadable(decks: readonly PlaybackDeck[]): boolean {
  return decks.some(deck => deck.loaded && deck.elapsed_seconds !== null);
}

/**
 * The deck MASTER follows: the only deck playing; with several, the one already
 * followed while it still plays, otherwise the longest-playing then furthest
 * into its track; with none, `fallback`.
 */
export function pickMaster(state: PlaybackState, followed: number | null, fallback: number): number {
  const playing = [...state.slots].filter(([, slot]) => slot.playing);
  if (followed !== null && playing.some(([slot]) => slot === followed)) return followed;
  if (!playing.length) return fallback;
  playing.sort(([aSlot, a], [bSlot, b]) => (a.since ?? 0) - (b.since ?? 0) || (b.elapsed ?? 0) - (a.elapsed ?? 0) || aSlot - bSlot);
  return playing[0][0];
}
