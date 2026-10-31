import type { DeckId, DeckStatus } from "../../types/dj-engine";

/** Below this window width (CSS px) Play switches to the waveform-only compact view. */
export const COMPACT_PLAY_MAX_WIDTH = 900;
export const COMPACT_PLAY_QUERY = `(max-width: ${COMPACT_PLAY_MAX_WIDTH - 1}px)`;
/** Height cap of one deck's waveform in the compact view; a taller window shows more below. */
export const COMPACT_WAVE_MAX_HEIGHT = 140;
/** Fixed rows of the compact view: the top bar, and the status line above each waveform. */
export const COMPACT_BAR_HEIGHT = 30;
export const COMPACT_STATUS_HEIGHT = 22;
/** Height of one row of deck pads (two decks side by side) below the waveforms. */
export const COMPACT_PADS_HEIGHT = 150;
/** The library appears only when it gets at least this much height. */
export const COMPACT_LIBRARY_MIN_HEIGHT = 220;

export type CompactTiers = { pads: boolean; library: boolean };

/** Height the compact view takes for `decks` waveforms at their cap. */
export function compactWavesHeight(decks: number): number {
  return COMPACT_BAR_HEIGHT + decks * (COMPACT_STATUS_HEIGHT + COMPACT_WAVE_MAX_HEIGHT);
}

/**
 * What the compact view shows below the capped waveforms for a window height:
 * the deck pads once a row per deck pair fits, then the library once it gets
 * COMPACT_LIBRARY_MIN_HEIGHT under the pads.
 */
export function compactTiers(height: number, decks: number): CompactTiers {
  const pads = compactWavesHeight(decks) + Math.ceil(decks / 2) * COMPACT_PADS_HEIGHT;
  return { pads: height >= pads, library: height >= pads + COMPACT_LIBRARY_MIN_HEIGHT };
}

/** Window size that fits the audio settings dialog while it is open from the compact view. */
export const COMPACT_AUDIO_SETTINGS_WIDTH = 640;
export const COMPACT_AUDIO_SETTINGS_HEIGHT = 820;
/** The browse menu hides this long after the last knob movement. */
export const COMPACT_MENU_HIDE_MS = 4000;
/** After a load the confirmation stays this long before the menu hides. */
export const COMPACT_MENU_LOADED_MS = 1500;

/** Controller library actions that drive the compact browse menu instead of the hidden library. */
const MENU_CONTROLS = new Set(["browse", "browsePress", "load", "back", "previous", "next"]);
export const COMPACT_LIBRARY_EVENT = "plumdeck:compact-library";
export const LIBRARY_EVENT = "plumdeck:controller-library";

/** Which window event a controller library action goes to. Outside the compact
 * view every action keeps going to the normal Play library. */
export function controllerLibraryEvent(control: string, compact: boolean): string {
  return compact && MENU_CONTROLS.has(control) ? COMPACT_LIBRARY_EVENT : LIBRARY_EVENT;
}

export const TEMPO_RANGE_EVENT = "plumdeck:controller-tempo-range";

/** The next tempo range after `range`, wrapping like the controller's TEMPO RANGE button. */
export function nextTempoRange<T extends number>(ranges: readonly T[], range: T): T {
  const index = ranges.indexOf(range);
  return ranges[(index + 1) % ranges.length];
}

/** Moves a list cursor by a signed knob delta, clamped to the loaded rows. */
export function stepCursor(index: number, delta: number, length: number): number {
  if (length <= 0) return 0;
  if (!Number.isFinite(delta) || delta === 0) return Math.max(0, Math.min(length - 1, index));
  return Math.max(0, Math.min(length - 1, index + Math.trunc(delta)));
}

type DeckLike = { status?: DeckStatus; track?: unknown } | null | undefined;

/**
 * Deck a knob press loads onto. The selected (active) deck is the convention
 * everywhere else in Play; the controller refuses to load onto a playing deck,
 * so a playing active deck falls back to an empty visible deck, then to any
 * visible deck that is not playing. Null when every visible deck is playing.
 */
export function knobLoadDeck(active: DeckId, visible: readonly DeckId[], decks: Partial<Record<DeckId, DeckLike>>): DeckId | null {
  const idle = (id: DeckId) => decks[id]?.status !== "playing";
  if (visible.includes(active) && idle(active)) return active;
  return visible.find((id) => idle(id) && !decks[id]?.track) ?? visible.find(idle) ?? null;
}

/** Remaining time like a CDJ: "-03:21". */
export function remainingText(positionMs: number, durationMs: number): string {
  if (!durationMs) return "--:--";
  const seconds = Math.max(0, Math.ceil((durationMs - Math.max(0, positionMs)) / 1000));
  return `-${String(Math.floor(seconds / 60)).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
}
