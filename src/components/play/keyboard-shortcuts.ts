/**
 * Play mode keyboard shortcuts. The default preset follows the widely used
 * "Performance 1" layout of DJ software; the previous plumdeck keys stay
 * selectable as "plumdeck クラシック". Keys are matched on `event.code` so the
 * physical layout works on JIS and US keyboards alike.
 * Pure module: runs under `node --test`.
 */

export type ShortcutPreset = "performance1" | "classic";
export const SHORTCUT_PRESETS: readonly { id: ShortcutPreset; label: string }[] = [
  { id: "performance1", label: "Performance 1" },
  { id: "classic", label: "plumdeck クラシック" },
];
export const SHORTCUT_STORAGE = "plumdeck.keyboardPreset";
export const readShortcutPreset = (value: string | null | undefined): ShortcutPreset => value === "classic" ? "classic" : "performance1";

/** Deck side: 0 = Deck 1 (left), 1 = Deck 2 (right). */
export type DeckSide = 0 | 1;
export type ShortcutAction =
  | { kind: "play" | "cue" | "sync" | "master" | "loopIn" | "loopOut" | "reloop" | "memoryPrevious" | "memoryNext" | "slipReverse"; side: DeckSide }
  | { kind: "bend"; side: DeckSide; direction: -1 | 1 }
  | { kind: "hotcue"; side: DeckSide; slot: number }
  | { kind: "load"; side: DeckSide }
  | { kind: "layout" }
  | { kind: "zoom"; direction: -1 | 1 }
  /** Classic preset only. */
  | { kind: "selectDeck"; deck: number }
  | { kind: "togglePlayActive" }
  | { kind: "seekActive"; deltaMs: number };

export type KeyInput = { code: string; shiftKey?: boolean; altKey?: boolean; ctrlKey?: boolean; metaKey?: boolean };

/** Momentary actions also need the key-up. */
export const isMomentary = (action: ShortcutAction) => action.kind === "bend" || action.kind === "slipReverse" || action.kind === "cue";

type Entry = { code: string; shift?: boolean; action: ShortcutAction; label: string; key: string };

const deckEntries = (side: DeckSide, keys: Record<string, string>): Entry[] => {
  const code = (key: string) => key === ";" ? "Semicolon" : /^\d$/.test(key) ? `Digit${key}` : `Key${key}`;
  const entries: Entry[] = [
    { code: code(keys.play), action: { kind: "play", side }, label: "再生 / 一時停止", key: keys.play },
    { code: code(keys.cue), action: { kind: "cue", side }, label: "CUE", key: keys.cue },
    { code: code(keys.sync), action: { kind: "sync", side }, label: "BEAT SYNC", key: keys.sync },
    { code: code(keys.sync), shift: true, action: { kind: "master", side }, label: "SYNC MASTER", key: `Shift+${keys.sync}` },
    { code: code(keys.bendDown), action: { kind: "bend", side, direction: -1 }, label: "ピッチベンド −", key: keys.bendDown },
    { code: code(keys.bendUp), action: { kind: "bend", side, direction: 1 }, label: "ピッチベンド +", key: keys.bendUp },
    { code: code(keys.loopIn), action: { kind: "loopIn", side }, label: "LOOP IN", key: keys.loopIn },
    { code: code(keys.loopOut), action: { kind: "loopOut", side }, label: "LOOP OUT", key: keys.loopOut },
    { code: code(keys.reloop), action: { kind: "reloop", side }, label: "RELOOP / EXIT", key: keys.reloop },
    { code: code(keys.memoryPrevious), action: { kind: "memoryPrevious", side }, label: "MEMORY CUE 前", key: keys.memoryPrevious },
    { code: code(keys.memoryNext), action: { kind: "memoryNext", side }, label: "MEMORY CUE 次", key: keys.memoryNext },
    { code: code(keys.slipReverse), action: { kind: "slipReverse", side }, label: "SLIP REVERSE", key: keys.slipReverse },
  ];
  keys.hotcues.split("").forEach((key, slot) => entries.push({ code: code(key), action: { kind: "hotcue", side, slot }, label: `HOT CUE ${String.fromCharCode(65 + slot)}`, key }));
  return entries;
};

const PERFORMANCE_1: Entry[] = [
  ...deckEntries(0, { play: "Z", cue: "A", sync: "Q", bendDown: "W", bendUp: "E", loopIn: "D", loopOut: "F", reloop: "G", memoryPrevious: "R", memoryNext: "T", slipReverse: "S", hotcues: "1234" }),
  ...deckEntries(1, { play: "N", cue: "H", sync: "Y", bendDown: "U", bendUp: "I", loopIn: "K", loopOut: "L", reloop: ";", memoryPrevious: "O", memoryNext: "P", slipReverse: "J", hotcues: "6789" }),
  { code: "Space", action: { kind: "layout" }, label: "レイアウト切替（ブラウザを広く）", key: "Space" },
  { code: "ArrowLeft", shift: true, action: { kind: "load", side: 0 }, label: "選択中の曲を Deck 1 へロード", key: "Shift+←" },
  { code: "ArrowRight", shift: true, action: { kind: "load", side: 1 }, label: "選択中の曲を Deck 2 へロード", key: "Shift+→" },
  { code: "Equal", action: { kind: "zoom", direction: 1 }, label: "波形ズームイン", key: "=" },
  { code: "Minus", action: { kind: "zoom", direction: -1 }, label: "波形ズームアウト", key: "-" },
];

const CLASSIC: Entry[] = [
  ...[1, 2, 3, 4].map((deck) => ({ code: `Digit${deck}`, action: { kind: "selectDeck", deck: deck - 1 } as ShortcutAction, label: "デッキ選択", key: String(deck) })),
  { code: "Space", action: { kind: "togglePlayActive" }, label: "再生 / 一時停止", key: "Space" },
  { code: "KeyQ", action: { kind: "seekActive", deltaMs: -5_000 }, label: "5 秒戻る", key: "Q" },
  { code: "KeyE", action: { kind: "seekActive", deltaMs: 5_000 }, label: "5 秒進む", key: "E" },
];

export const shortcutTable = (preset: ShortcutPreset): readonly Entry[] => preset === "classic" ? CLASSIC : PERFORMANCE_1;

/** The action for a key press, or null. Modifier keys other than Shift never match. */
export function resolveShortcut(preset: ShortcutPreset, input: KeyInput): ShortcutAction | null {
  if (input.altKey || input.ctrlKey || input.metaKey) return null;
  const shift = Boolean(input.shiftKey);
  const entry = shortcutTable(preset).find((item) => item.code === input.code && Boolean(item.shift) === shift);
  return entry?.action ?? null;
}

/** Help table rows: deck operations side by side (Deck 1 / Deck 2), then the rest. */
export function shortcutHelp(preset: ShortcutPreset): { label: string; keys: [string | null, string | null] | string }[] {
  const rows = new Map<string, [string | null, string | null]>();
  const other: { label: string; keys: string }[] = [];
  for (const entry of shortcutTable(preset)) {
    if ("side" in entry.action && entry.action.kind !== "load") {
      const keys = rows.get(entry.label) ?? [null, null];
      keys[entry.action.side] = entry.key;
      rows.set(entry.label, keys);
    } else other.push({ label: entry.label, keys: entry.key });
  }
  return [...[...rows].map(([label, keys]) => ({ label, keys })), ...other];
}
