import { assetIdentity } from '../junction/asset-resolver';
import { junctionLeaseKey } from '../junction/state';
import type { DeckId } from "../../types/dj-engine";
import { djEngineClient } from "./client";
import { apiClient } from "../api-client";

/**
 * MEMORY CUEs. Library tracks keep them in the database (performance
 * metadata in the Rust backend); shared Junction assets, which have no
 * library row, keep them in this browser as before. Entries the old build
 * left in localStorage are moved to the database once.
 */
export type MemoryCue = { positionMs: number; endMs?: number; label?: string; source?: "user" | "rekordbox" };
type ApiCue = { position_ms: number; end_ms?: number | null; label?: string | null; source?: string };

const LEGACY_PREFIX = "plumdeck.memoryCues.";
const key = (track: string) => `${LEGACY_PREFIX}${track}`;
const MIGRATED = "plumdeck.memoryCues.migratedToDb.v1";
const MAX = 128;

const libraryId = (track: string) => /^\d+$/.test(track) ? Number(track) : null;
const cache = new Map<string, MemoryCue[]>();
const loading = new Map<string, Promise<MemoryCue[]>>();

const fromApi = (cues: ApiCue[]): MemoryCue[] => cues.filter(c => Number.isFinite(c.position_ms)).map(c => ({
  positionMs: c.position_ms, ...(typeof c.end_ms === "number" ? { endMs: c.end_ms } : {}), ...(c.label ? { label: c.label } : {}),
  source: c.source === "rekordbox" ? "rekordbox" as const : "user" as const,
}));
const toApi = (cues: MemoryCue[]): ApiCue[] => cues.map(c => ({ position_ms: c.positionMs, ...(c.endMs !== undefined ? { end_ms: c.endMs } : {}), ...(c.label ? { label: c.label } : {}), source: c.source ?? "user" }));

function changed(trackId: string) {
  window.dispatchEvent(new CustomEvent("plumdeck:memory-cues", { detail: { trackId } }));
}

function readLegacy(track: string): MemoryCue[] {
  try {
    const data = JSON.parse(localStorage.getItem(key(track)) ?? "[]");
    return Array.isArray(data) ? data.filter((v): v is MemoryCue => v && Number.isFinite(v.positionMs) && v.positionMs >= 0 && (v.endMs === undefined || Number.isFinite(v.endMs) && v.endMs > v.positionMs)) : [];
  } catch { return []; }
}

function sanitize(points: MemoryCue[], duration: number) {
  return points.filter(v => v.positionMs >= 0 && v.positionMs < duration && (v.endMs === undefined || v.endMs > v.positionMs && v.endMs <= duration))
    .slice(0, MAX).sort((a, b) => a.positionMs - b.positionMs);
}

function merge(base: MemoryCue[], extra: MemoryCue[]) {
  const out = [...base];
  for (const cue of extra) if (!out.some(c => Math.abs(c.positionMs - cue.positionMs) <= 10 && (c.endMs === undefined) === (cue.endMs === undefined))) out.push(cue);
  return out.sort((a, b) => a.positionMs - b.positionMs).slice(0, MAX);
}

async function load(track: string): Promise<MemoryCue[]> {
  const id = libraryId(track);
  if (id === null) return readLegacy(track);
  const answer = await apiClient.get<{ cues: ApiCue[] }>(`/tracks/${id}/memory-cues`);
  let cues = fromApi(answer.cues ?? []);
  // A track the old build stored locally moves to the database on first use.
  const legacy = readLegacy(track);
  if (legacy.length) {
    const merged = merge(cues, legacy);
    const saved = await apiClient.put<{ cues: ApiCue[] }>(`/tracks/${id}/memory-cues`, { cues: toApi(merged) });
    cues = fromApi(saved.cues ?? []);
    try { localStorage.removeItem(key(track)); } catch { /* keep: merged again next time, deduplicated */ }
  }
  return cues;
}

/** Loads (once) and caches a track's memory cues; notifies when they arrive. */
export function ensureMemoryCues(track: string): Promise<MemoryCue[]> {
  const hit = loading.get(track);
  if (hit) return hit;
  const request = load(track).then(cues => { cache.set(track, cues); changed(track); return cues; });
  loading.set(track, request);
  void request.catch(() => loading.delete(track));
  return request;
}

/** Cached memory cues (empty until loaded; `ensureMemoryCues` fetches them). */
export function memoryCues(track: string, duration = Infinity): MemoryCue[] {
  const cues = cache.get(track);
  if (!cues) { void ensureMemoryCues(track).catch(() => undefined); return []; }
  return sanitize(cues, duration);
}

/** Drops cached cues (after a rekordbox import) so the next read refetches. */
export function invalidateMemoryCues(track?: string) {
  if (track === undefined) { cache.clear(); loading.clear(); } else { cache.delete(track); loading.delete(track); }
  changed(track ?? "*");
}

async function store(trackId: string, points: MemoryCue[]) {
  const sorted = [...points].sort((a, b) => a.positionMs - b.positionMs);
  const id = libraryId(trackId);
  const previous = cache.get(trackId);
  cache.set(trackId, sorted); changed(trackId);
  if (id === null) { try { localStorage.setItem(key(trackId), JSON.stringify(sorted)); } catch { /* memory only */ } return; }
  try {
    const saved = await apiClient.put<{ cues: ApiCue[] }>(`/tracks/${id}/memory-cues`, { cues: toApi(sorted) });
    cache.set(trackId, fromApi(saved.cues ?? [])); changed(trackId);
  } catch (error) {
    if (previous) cache.set(trackId, previous); else cache.delete(trackId);
    changed(trackId);
    throw new Error(`メモリーキューを保存できませんでした: ${error instanceof Error ? error.message : String(error)}`);
  }
}

/**
 * One-time move of every memory cue the old build kept in localStorage
 * (library tracks only) into the database.
 */
export async function migrateLegacyMemoryCues(): Promise<number> {
  try { if (localStorage.getItem(MIGRATED)) return 0; } catch { return 0; }
  const tracks: string[] = [];
  try {
    for (let i = 0; i < localStorage.length; i++) {
      const name = localStorage.key(i);
      if (name?.startsWith(LEGACY_PREFIX) && /^\d+$/.test(name.slice(LEGACY_PREFIX.length))) tracks.push(name.slice(LEGACY_PREFIX.length));
    }
  } catch { return 0; }
  let moved = 0;
  for (const track of tracks) {
    try { loading.delete(track); await ensureMemoryCues(track); moved++; } catch { /* left in place; retried on next use */ }
  }
  try { if (tracks.every(track => !localStorage.getItem(key(track)))) localStorage.setItem(MIGRATED, "1"); } catch { /* retried next start */ }
  return moved;
}

/** The memory cue last called on each deck. DELETE removes this one. */
const selected: Partial<Record<DeckId, { trackId: string; positionMs: number }>> = {};
export function selectedMemoryCue(deck: DeckId, trackId: string | undefined): number | null {
  const current = selected[deck];
  return current && current.trackId === trackId ? current.positionMs : null;
}
function select(deck: DeckId, trackId: string, positionMs: number | null) {
  if (positionMs === null) delete selected[deck]; else selected[deck] = { trackId, positionMs };
  changed(trackId);
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
  let points = sanitize(await ensureMemoryCues(trackId).catch(() => memoryCues(trackId, durationMs)), durationMs);
  if (action === "save") {
    const cue: MemoryCue = state.loopRegion?.enabled ? { positionMs: state.loopRegion.startMs, endMs: state.loopRegion.endMs, source: "user" } : { positionMs: Math.max(0, state.positionMs), source: "user" };
    if (cue.positionMs >= durationMs) return;
    points = points.filter(p => Math.abs(p.positionMs-cue.positionMs)>10);
    if (points.length >= MAX) throw new Error(`メモリーキューは1曲${MAX}件までです`);
    points.push(cue);
    select(deck, trackId, cue.positionMs);
    await store(trackId, points);
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
    select(deck, trackId, null);
    await store(trackId, points.filter(p => Math.abs(p.positionMs - target) > 1));
    return;
  }
  const point = action === "next" ? points.find(p => p.positionMs>state.positionMs+10) : [...points].reverse().find(p => p.positionMs<state.positionMs-10);
  if (!point) return;
  select(deck, trackId, point.positionMs);
  await call(deck, point, false);
}
