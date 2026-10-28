import { assetIdentity } from '../junction/asset-resolver';
import { junctionLeaseKey } from '../junction/state';
import type { DeckId } from "../../types/dj-engine";
import { djEngineClient } from "./client";
export type MemoryCue = { positionMs: number; endMs?: number };
const key = (track: string) => `plumdeck.memoryCues.${track}`;
export function memoryCues(track: string, duration = Infinity): MemoryCue[] {
  try { const data = JSON.parse(localStorage.getItem(key(track)) ?? "[]"); return Array.isArray(data) ? data.filter((v): v is MemoryCue => v && Number.isFinite(v.positionMs) && v.positionMs >= 0 && v.positionMs < duration && (v.endMs === undefined || Number.isFinite(v.endMs) && v.endMs > v.positionMs && v.endMs <= duration)).slice(0,64).sort((a,b) => a.positionMs-b.positionMs) : []; } catch { return []; }
}

/** The memory cue last called on each deck. DELETE removes this one. */
const selected: Partial<Record<DeckId, { trackId: string; positionMs: number }>> = {};
export function selectedMemoryCue(deck: DeckId, trackId: string | undefined): number | null {
  const current = selected[deck];
  return current && current.trackId === trackId ? current.positionMs : null;
}
function select(deck: DeckId, trackId: string, positionMs: number | null) {
  if (positionMs === null) delete selected[deck]; else selected[deck] = { trackId, positionMs };
  window.dispatchEvent(new CustomEvent("plumdeck:memory-cues", {detail:{trackId}}));
}

function store(trackId: string, points: MemoryCue[]) {
  localStorage.setItem(key(trackId), JSON.stringify(points.sort((a,b) => a.positionMs-b.positionMs)));
  window.dispatchEvent(new CustomEvent("plumdeck:memory-cues", {detail:{trackId}}));
}

/** Jump to a memory cue (re-arming its loop). `pause` follows the CDJ CALL: stop at the cue. */
async function call(deck: DeckId, point: MemoryCue, pause: boolean) {
  const lease = junctionLeaseKey();
  const session = djEngineClient.getSessionId(), generation = djEngineClient.getDeckGeneration(deck);
  const valid = () => lease === junctionLeaseKey() && session === djEngineClient.getSessionId() && generation === djEngineClient.getDeckGeneration(deck);
  if (pause && djEngineClient.getState().snapshot?.decks[deck]?.status === "playing") await djEngineClient.pause(deck);
  if (!valid()) return;
  await djEngineClient.seek(deck, point.positionMs);
  if (point.endMs !== undefined && valid()) { await djEngineClient.setLoop(deck, point.positionMs, point.endMs); if (valid()) await djEngineClient.enableLoop(deck, true); }
}

/** MEMORY CUE pad: call the `index`-th cue, stop there and select it. Returns the cue. */
export async function callMemoryCue(deck: DeckId, index: number): Promise<MemoryCue | null> {
  const state = djEngineClient.getState().snapshot?.decks[deck];
  if (!state?.track) return null;
  const trackId = assetIdentity(state.track);
  const point = memoryCues(trackId, state.track.durationMs)[index];
  if (!point) return null;
  select(deck, trackId, point.positionMs);
  await call(deck, point, true);
  return point;
}

export async function memoryAction(deck: DeckId, action: "save" | "delete" | "previous" | "next") {
  const state = djEngineClient.getState().snapshot?.decks[deck];
  if (!state?.track) return;
  const {durationMs} = state.track;
  const trackId = assetIdentity(state.track);
  let points = memoryCues(trackId, durationMs);
  if (action === "save") {
    const cue: MemoryCue = state.loopRegion?.enabled ? { positionMs: state.loopRegion.startMs, endMs: state.loopRegion.endMs } : {positionMs:Math.max(0,state.positionMs)};
    if (cue.positionMs >= durationMs) return;
    points = points.filter(p => Math.abs(p.positionMs-cue.positionMs)>10);
    if (points.length >= 64) throw new Error("メモリーキューは1曲64件までです");
    points.push(cue);
    store(trackId, points);
    select(deck, trackId, cue.positionMs);
    return;
  }
  if (action === "delete") {
    // As on a CDJ: delete the called cue while the deck stands on it.
    // Otherwise the cue at the playhead (within 50 ms), as before; a cue
    // called minutes ago is never deleted from far away.
    const near = (positionMs: number) => Math.abs(positionMs - state.positionMs) <= 50;
    const called = selectedMemoryCue(deck, trackId);
    const target = called !== null && state.status !== "playing" && near(called) ? called : points.find(p => near(p.positionMs))?.positionMs ?? null;
    if (target === null) throw new Error("削除するメモリーキューの位置で止めてください（パッドや ◀ ▶ で呼び出すとその位置に移動します）");
    store(trackId, points.filter(p => Math.abs(p.positionMs - target) > 1));
    select(deck, trackId, null);
    return;
  }
  const point = action === "next" ? points.find(p => p.positionMs>state.positionMs+10) : points.reverse().find(p => p.positionMs<state.positionMs-10);
  if (!point) return;
  select(deck, trackId, point.positionMs);
  await call(deck, point, false);
}
