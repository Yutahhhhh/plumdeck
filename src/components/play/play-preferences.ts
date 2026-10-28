import { useSyncExternalStore } from "react";
import { KEY_NOTATION_STORAGE, readKeyNotation, type KeyNotation } from "@/services/dj-engine/musical-key";
import { COLOR_FX_STORAGE, CROSSFADER_CURVE_STORAGE, FADER_CURVE_STORAGE, readColorFx, readCrossfaderCurve, readFaderCurve, type ColorFxEffect, type CrossfaderCurveOption, type FaderCurve } from "./mixer-math";
import { readShortcutPreset, SHORTCUT_STORAGE, type ShortcutPreset } from "./keyboard-shortcuts";

/** Per-viewer Play settings. Storage can be unavailable; defaults always apply. */
export type PlayPreferences = { keyNotation: KeyNotation; faderCurve: FaderCurve; crossfaderCurve: CrossfaderCurveOption; colorFx: ColorFxEffect; keyboard: ShortcutPreset };

const KEYS: Record<keyof PlayPreferences, string> = { keyNotation: KEY_NOTATION_STORAGE, faderCurve: FADER_CURVE_STORAGE, crossfaderCurve: CROSSFADER_CURVE_STORAGE, colorFx: COLOR_FX_STORAGE, keyboard: SHORTCUT_STORAGE };

function read(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}

let current: PlayPreferences = {
  keyNotation: readKeyNotation(read(KEYS.keyNotation)),
  faderCurve: readFaderCurve(read(KEYS.faderCurve)),
  crossfaderCurve: readCrossfaderCurve(read(KEYS.crossfaderCurve)),
  colorFx: readColorFx(read(KEYS.colorFx)),
  // Performance 1 by default; the previous keys stay selectable as "plumdeck クラシック".
  keyboard: readShortcutPreset(read(KEYS.keyboard)),
};
const listeners = new Set<() => void>();

export function setPlayPreference<K extends keyof PlayPreferences>(key: K, value: PlayPreferences[K]) {
  if (current[key] === value) return;
  current = { ...current, [key]: value };
  try { localStorage.setItem(KEYS[key], String(value)); } catch { /* keep the in-memory value */ }
  for (const listener of listeners) listener();
}

export const getPlayPreferences = () => current;
const subscribe = (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener); }; };

export function usePlayPreferences(): PlayPreferences {
  return useSyncExternalStore(subscribe, getPlayPreferences);
}
