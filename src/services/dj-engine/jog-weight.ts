import type { DeckId } from "../../types/dj-engine.ts";

/** 0 = LIGHT（バックスピンが長く回る）、1 = HEAVY（強く止まる）。デッキごとに保存する。 */
export const JOG_WEIGHT_DEFAULT = 0.5;

const key = (deck: DeckId) => `plumdeck.jogWeight.${deck}`;
const cache = new Map<DeckId, number>();
const listeners = new Set<() => void>();

function normalize(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : JOG_WEIGHT_DEFAULT;
}

export function jogWeight(deck: DeckId): number {
  const cached = cache.get(deck);
  if (cached !== undefined) return cached;
  let value = JOG_WEIGHT_DEFAULT;
  try {
    const stored = globalThis.localStorage?.getItem(key(deck));
    if (stored !== null && stored !== undefined && stored !== "") value = normalize(Number(stored));
  } catch { /* ストレージが使えない環境では既定値 */ }
  cache.set(deck, value);
  return value;
}

export function setJogWeight(deck: DeckId, value: number): void {
  const next = normalize(value);
  if (cache.get(deck) === next) return;
  cache.set(deck, next);
  try { globalThis.localStorage?.setItem(key(deck), String(next)); } catch { /* 保存できなくても今回の値は使う */ }
  for (const listener of listeners) listener();
}

export function subscribeJogWeight(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function jogWeightLabel(value: number): string {
  return value < 0.34 ? "LIGHT" : value > 0.66 ? "HEAVY" : "MEDIUM";
}
