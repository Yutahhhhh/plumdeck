/**
 * Musical key parsing and notation for the deck displays.
 *
 * The engine does not report a usable key (`musicalKey` is always 0 on the
 * Mixxx host), so the current key is derived from the library key plus the
 * deck's key shift. Pure module: runs under `node --test`.
 */

export type KeyNotation = "camelot" | "classic";
export const KEY_NOTATIONS: readonly KeyNotation[] = ["camelot", "classic"];
export const KEY_NOTATION_STORAGE = "plumdeck.keyNotation";

/** pitch: 0 = C … 11 = B. */
export type MusicalKey = { pitch: number; minor: boolean };

const NOTE_PITCH: Record<string, number> = { c: 0, d: 2, e: 4, f: 5, g: 7, a: 9, b: 11 };
const MAJOR_NAMES = ["C", "Db", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];
const MINOR_NAMES = ["Cm", "C#m", "Dm", "Ebm", "Em", "Fm", "F#m", "Gm", "G#m", "Am", "Bbm", "Bm"];

const mod12 = (value: number) => ((value % 12) + 12) % 12;

/** Camelot number of a major key: C = 8B, each fifth up adds one. */
const majorCamelot = (pitch: number) => (mod12(pitch * 7) + 7) % 12 + 1;

export function camelotOf(key: MusicalKey): { number: number; letter: "A" | "B" } {
  return key.minor ? { number: majorCamelot(key.pitch + 3), letter: "A" } : { number: majorCamelot(key.pitch), letter: "B" };
}

/** Accepts "F Minor", "f# major", "Am", "Bbm", "D♭", "C maj", "8A", "12b". */
export function parseKey(text: string | null | undefined): MusicalKey | null {
  const raw = (text ?? "").trim();
  if (!raw) return null;
  const camelot = /^(\d{1,2})\s*([ab])$/i.exec(raw);
  if (camelot) {
    const number = Number(camelot[1]);
    if (number < 1 || number > 12) return null;
    // Invert majorCamelot: 7 is its own inverse modulo 12.
    const major = mod12(7 * (number - 8));
    return camelot[2].toLowerCase() === "b" ? { pitch: major, minor: false } : { pitch: mod12(major - 3), minor: true };
  }
  const classic = /^([a-g])\s*(#|♯|b|♭)?\s*(.*)$/i.exec(raw);
  if (!classic) return null;
  let pitch = NOTE_PITCH[classic[1].toLowerCase()];
  if (classic[2] === "#" || classic[2] === "♯") pitch += 1;
  if (classic[2] === "b" || classic[2] === "♭") pitch -= 1;
  const quality = classic[3].trim().toLowerCase();
  let minor: boolean;
  if (quality === "" || /^(maj|major|ma|dur)$/.test(quality)) minor = false;
  else if (/^(m|min|minor|moll|-)$/.test(quality)) minor = true;
  else return null;
  return { pitch: mod12(pitch), minor };
}

export function shiftKey(key: MusicalKey, semitones: number): MusicalKey {
  return { pitch: mod12(key.pitch + Math.round(semitones)), minor: key.minor };
}

export function formatKey(key: MusicalKey, notation: KeyNotation): string {
  if (notation === "classic") return (key.minor ? MINOR_NAMES : MAJOR_NAMES)[key.pitch];
  const { number, letter } = camelotOf(key);
  return `${number}${letter}`;
}

/** Library value in the chosen notation; unknown text is shown as-is. */
export function formatLibraryKey(text: string | null | undefined, notation: KeyNotation): string {
  const key = parseKey(text);
  return key ? formatKey(key, notation) : (text ?? "").trim();
}

/** Pitch movement caused by playing at `rate` without master tempo. */
export const tempoSemitones = (rate: number) => rate > 0 && Number.isFinite(rate) ? 12 * Math.log2(rate) : 0;

export type CurrentKey = {
  /** Display text in the chosen notation, or null when the original key is unknown. */
  text: string | null;
  /** Total offset from the original key in semitones (key shift plus tempo when MT is off). */
  delta: number;
  /** True when the tempo moves the pitch off the semitone grid (MT off). */
  approximate: boolean;
};

/**
 * Current key = library key + key shift. With MT off the tempo change also
 * moves the pitch; it is added to the nearest semitone and marked approximate.
 */
export function currentKey(libraryKey: string | null | undefined, keyShift: number, rate: number, keylock: boolean, notation: KeyNotation): CurrentKey {
  const shift = Number.isFinite(keyShift) ? Math.round(keyShift) : 0;
  const tempo = keylock ? 0 : tempoSemitones(rate);
  const approximate = !keylock && Math.abs(tempo) >= 0.05;
  const delta = shift + (approximate ? Math.round(tempo) : 0);
  const key = parseKey(libraryKey);
  return { text: key ? formatKey(shiftKey(key, delta), notation) : null, delta, approximate };
}

/** "+2", "−3", "±0" (U+2212 minus like the rest of the deck readouts). */
export const formatKeyDelta = (delta: number) => delta === 0 ? "±0" : `${delta > 0 ? "+" : "−"}${Math.abs(delta)}`;

export function readKeyNotation(value: string | null | undefined): KeyNotation {
  return value === "classic" ? "classic" : "camelot";
}
