import type { DeckId } from "../../types/dj-engine";

// Official mode-button NOTE addresses, in the same order as pad note >> 4.
export const PAD_MODE_NOTES = [0x1b, 0x1e, 0x20, 0x22, 0x69, 0x6b, 0x6d, 0x6f] as const;
export const PAD_MODE_NAMES = ["hotcue", "padfx", "beatjump", "sampler", "keyboard", "padfx2", "beatloop", "keyshift"] as const;
export type PadSelection = { mode: number; page: number; software?: boolean };
/** Last page per mode. Controller pads use pages 0/1; BEAT JUMP (FINE) and KEY SHIFT/KEYBOARD (±12) have more on screen. */
export const PAD_MODE_LAST_PAGE = [1, 1, 2, 1, 3, 1, 1, 3] as const;
const selections: Record<DeckId, PadSelection> = { A: {mode:0,page:0}, B: {mode:0,page:0}, C: {mode:0,page:0}, D: {mode:0,page:0} };
const listeners = new Set<() => void>();
export const subscribePads = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };
export const getPadSelection = (deck: DeckId) => selections[deck];
export function selectPads(deck: DeckId, mode: number, page = 0, software = false) {
  if (!Number.isInteger(mode) || mode < 0 || mode > 7) return;
  page = Math.max(0, Math.min(PAD_MODE_LAST_PAGE[mode], page));
  if (selections[deck].mode === mode && selections[deck].page === page && Boolean(selections[deck].software) === software) return;
  selections[deck] = {mode,page,software};
  for (const listener of listeners) listener();
}
export function padSelectionFeedback(): number[][] {
  return (["A", "B", "C", "D"] as const).flatMap((deck, ch) => {
    const {mode,page} = selections[deck];
    return [...PAD_MODE_NOTES.map((note, index) => [0x90+ch,note,index===mode?127:0]),
      [0x90+ch,0x24+mode,page===0?127:0], [0x90+ch,0x2c+mode,page>0?127:0]];
  });
}
