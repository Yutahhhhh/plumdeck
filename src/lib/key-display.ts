/**
 * Key notation, parsing and compatibility shared by every screen (RB-03..RB-06).
 * The table and the relation matrix are generated from plumdeck-key
 * (keys.generated.ts); this module adds parsing of stored spellings, the
 * user's notation setting, the master deck and the Traffic Light level.
 */
import { useEffect, useState } from "react";
import {
  DEFAULT_KEY_NOTATION,
  KEYS,
  TRAFFIC_LIGHT,
  formatKeyId,
  keyRelation,
  type KeyNotationId,
  type KeyRelation,
} from "./keys.generated";

const NOTATION_STORAGE = "plumdeck.keyNotation";
const FILTER_STORAGE = "plumdeck.keyFilter";
const LEVEL2_STORAGE = "plumdeck.trafficLightLevel2";
const CHANGE_EVENT = "plumdeck:key-display";

const PITCH: Record<string, number> = { C: 0, D: 2, E: 4, F: 5, G: 7, A: 9, B: 11 };

/** Any accepted spelling ("A minor", "Am", "a min", "8A", "1m", "G#m", "B♭") as a KeyId 0..23. */
export function parseKey(raw: string | null | undefined): number | null {
  if (!raw) return null;
  const text = raw.trim().replace(/♯/g, "#").replace(/♭/g, "b");
  if (!text) return null;
  const alpha = /^(1[0-2]|[1-9])\s*([ABab])$/.exec(text);
  if (alpha) {
    const code = `${alpha[1]}${alpha[2].toUpperCase()}`;
    const found = KEYS.find((key) => key.alphanumeric === code);
    return found ? found.id : null;
  }
  const open = /^(1[0-2]|[1-9])\s*([dmDM])$/.exec(text);
  if (open) {
    const code = `${open[1]}${open[2].toLowerCase()}`;
    const found = KEYS.find((key) => key.open_key === code);
    return found ? found.id : null;
  }
  const classic = /^([A-Ga-g])\s*([#b]?)\s*(major|minor|maj|min|m|M)?$/.exec(text);
  if (!classic) return null;
  const letter = classic[1].toUpperCase();
  let pitch = PITCH[letter];
  if (classic[2] === "#") pitch += 1;
  if (classic[2] === "b") pitch -= 1;
  const quality = classic[3];
  const minor = quality === "m" || /^min/i.test(quality ?? "");
  return ((pitch % 12) + 12) % 12 + (minor ? 12 : 0);
}

function read(storage: string): string | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage.getItem(storage);
  } catch {
    return null;
  }
}

function write(storage: string, value: string) {
  try {
    localStorage.setItem(storage, value);
  } catch {
    // A private window keeps the choice for this session only.
  }
  if (typeof window !== "undefined") window.dispatchEvent(new Event(CHANGE_EVENT));
}

export function keyNotation(): KeyNotationId {
  const stored = read(NOTATION_STORAGE);
  return stored === "classic" || stored === "open_key" || stored === "alphanumeric" ? stored : DEFAULT_KEY_NOTATION;
}

export function setKeyNotation(notation: KeyNotationId) {
  write(NOTATION_STORAGE, notation);
}

/** Whether the recommendation key filter is on (off by default, remembered). */
export function keyFilterEnabled(): boolean {
  return read(FILTER_STORAGE) === "1";
}

export function setKeyFilterEnabled(on: boolean) {
  write(FILTER_STORAGE, on ? "1" : "0");
}

export function trafficLightLevel2(): boolean {
  return read(LEVEL2_STORAGE) === "1";
}

export function setTrafficLightLevel2(on: boolean) {
  write(LEVEL2_STORAGE, on ? "1" : "0");
}

/** Re-render when any key display setting changes. */
export function useKeyDisplay() {
  const [state, setState] = useState(() => ({ notation: keyNotation(), filter: keyFilterEnabled(), level2: trafficLightLevel2() }));
  useEffect(() => {
    const update = () => setState({ notation: keyNotation(), filter: keyFilterEnabled(), level2: trafficLightLevel2() });
    window.addEventListener(CHANGE_EVENT, update);
    window.addEventListener("storage", update);
    return () => {
      window.removeEventListener(CHANGE_EVENT, update);
      window.removeEventListener("storage", update);
    };
  }, []);
  return state;
}

/** A stored key in the chosen notation; the raw text when it cannot be read, "—" when empty. */
export function formatKey(raw: string | null | undefined, notation: KeyNotationId = keyNotation()): string {
  if (!raw || !raw.trim()) return "—";
  const id = parseKey(raw);
  return id === null ? raw : formatKeyId(id, notation);
}

/** Rotate a key by semitones (KEY SHIFT), keeping major/minor. */
export function shiftKey(id: number, semitones: number): number {
  const minor = id >= 12;
  const pitch = (((id % 12) + Math.round(semitones)) % 12 + 12) % 12;
  return pitch + (minor ? 12 : 0);
}

export function relationOf(base: number | null, candidate: string | null | undefined): KeyRelation | null {
  const id = parseKey(candidate);
  if (base === null || id === null) return null;
  return keyRelation(base, id);
}

/** Traffic Light level of a relation: 1 green, 2 yellow (when enabled), null none. */
export function trafficLevel(relation: KeyRelation | null, level2 = trafficLightLevel2()): 1 | 2 | null {
  if (!relation) return null;
  const level = TRAFFIC_LIGHT[relation];
  return level === 2 && !level2 ? null : level;
}

export const RELATION_LABELS: Record<KeyRelation, string> = {
  same: "同じキー",
  adjacent: "±1",
  relative: "平行調",
  diagonal: "対角",
  plus2: "+2",
  semitone: "半音",
  other: "",
};

/** The filter rule: same / adjacent / relative only, same first, unknown last. */
export function filterByKey<T extends { key?: string | null }>(items: T[], base: number | null): T[] {
  if (base === null) return items;
  const rank = (item: T) => {
    const relation = relationOf(base, item.key);
    if (relation === null) return 2;
    if (relation === "same") return 0;
    return relation === "adjacent" || relation === "relative" ? 1 : -1;
  };
  return items
    .map((item, index) => ({ item, index, rank: rank(item) }))
    .filter((entry) => entry.rank >= 0)
    .sort((a, b) => a.rank - b.rank || a.index - b.index)
    .map((entry) => entry.item);
}

type DeckLike = {
  track: { musicalKey?: string; trackId?: string } | null;
  status: string;
  syncLeader: string | null;
  keyShift?: number;
} | undefined;

/**
 * The MASTER deck: the sync leader, else a playing deck (the earliest in
 * `playOrder`), else the last loaded deck (`loadOrder`, newest last).
 */
export function masterDeck<Id extends string>(decks: readonly Id[], states: Partial<Record<Id, DeckLike>>, playOrder: readonly Id[] = [], loadOrder: readonly Id[] = []): Id | undefined {
  const loaded = decks.filter((deck) => states[deck]?.track);
  return loaded.find((deck) => states[deck]?.syncLeader === deck)
    ?? playOrder.find((deck) => loaded.includes(deck) && states[deck]?.status === "playing")
    ?? loaded.find((deck) => states[deck]?.status === "playing")
    ?? [...loadOrder].reverse().find((deck) => loaded.includes(deck))
    ?? loaded[0];
}

/** The deck's current key with KEY SHIFT (candidates are judged as key-locked). */
export function deckKey(state: DeckLike): number | null {
  const base = parseKey(state?.track?.musicalKey);
  if (base === null) return null;
  return shiftKey(base, state?.keyShift ?? 0);
}
