import { useEffect, useState } from "react";
import { apiClient } from "@/services/api-client";
import { parsePhrases, parseThreeBand, type Phrase, type ThreeBandLevel, type ThreeBandPayload, type ThreeBandWave } from "@/services/waveform/three-band";

// Overviews are small (1200 columns), so a browser page's worth stays
// resident; details are ~100 KB each and only decks ask for them.
const LIMITS: Record<ThreeBandLevel, number> = { overview: 400, detail: 8 };
const caches: Record<ThreeBandLevel, Map<number, Promise<ThreeBandWave | null>>> = { overview: new Map(), detail: new Map() };
let active = 0;
const waiting: (() => void)[] = [];

function gate<T>(work: () => Promise<T>, priority: boolean): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const begin = () => { active++; void work().then(resolve, reject).finally(() => { active--; waiting.shift()?.(); }); };
    if (active < 4) begin(); else if (priority) waiting.unshift(begin); else waiting.push(begin);
  });
}

export function fetchThreeBand(trackId: number, level: ThreeBandLevel): Promise<ThreeBandWave | null> {
  const cache = caches[level];
  const hit = cache.get(trackId);
  if (hit) { cache.delete(trackId); cache.set(trackId, hit); return hit; }
  const request = gate(() => apiClient.get<ThreeBandPayload>(`/tracks/${trackId}/waveform3`, { level }).then(parseThreeBand), level === "detail");
  cache.set(trackId, request);
  while (cache.size > LIMITS[level]) { const oldest = cache.keys().next().value; if (oldest === undefined) break; cache.delete(oldest); }
  void request.catch(() => { if (cache.get(trackId) === request) cache.delete(trackId); });
  return request;
}

/** The track's 3-band waveform (rekordbox when available, else plumdeck's own). */
export function useThreeBand(trackId: number | null, level: ThreeBandLevel) {
  const [state, setState] = useState<{ key: string; wave: ThreeBandWave | null; loading: boolean; error: string | null }>({ key: "", wave: null, loading: false, error: null });
  const [revision, setRevision] = useState(0);
  const key = trackId === null ? "" : `${trackId}:${level}`;
  useEffect(() => {
    if (trackId === null) return;
    let live = true;
    setState({ key, wave: null, loading: true, error: null });
    void fetchThreeBand(trackId, level).then(
      (wave) => { if (live) setState({ key, wave, loading: false, error: null }); },
      (error: unknown) => { if (live) setState({ key, wave: null, loading: false, error: error instanceof Error ? error.message : "波形を読み込めませんでした" }); },
    );
    return () => { live = false; };
  }, [key, revision]);
  const current = state.key === key ? state : { key, wave: null, loading: trackId !== null, error: null };
  return { ...current, retry: () => { if (trackId !== null) caches[level].delete(trackId); setRevision((v) => v + 1); } };
}

export type RekordboxTrack = { available: boolean; phrases: Phrase[]; mood: string | null };
const rbCache = new Map<number, Promise<RekordboxTrack>>();

export function fetchRekordboxTrack(trackId: number): Promise<RekordboxTrack> {
  const hit = rbCache.get(trackId);
  if (hit) return hit;
  const request = apiClient.get<{ available: boolean; phrases: { mood?: string; items?: unknown[] } | null }>(`/tracks/${trackId}/rekordbox`)
    .then((value) => ({ available: Boolean(value.available), phrases: parsePhrases(value.phrases), mood: value.phrases?.mood ?? null }));
  rbCache.set(trackId, request);
  if (rbCache.size > 32) { const oldest = rbCache.keys().next().value; if (oldest !== undefined) rbCache.delete(oldest); }
  void request.catch(() => { if (rbCache.get(trackId) === request) rbCache.delete(trackId); });
  return request;
}

const NONE: RekordboxTrack = { available: false, phrases: [], mood: null };

/** rekordbox phrases for a loaded track; tracks without them get none. */
export function useRekordboxTrack(trackId: number | null): RekordboxTrack {
  const [value, setValue] = useState<{ id: number | null; data: RekordboxTrack }>({ id: null, data: NONE });
  useEffect(() => {
    if (trackId === null) return;
    let live = true;
    void fetchRekordboxTrack(trackId).then((data) => { if (live) setValue({ id: trackId, data }); }, () => { if (live) setValue({ id: trackId, data: NONE }); });
    return () => { live = false; };
  }, [trackId]);
  return value.id === trackId ? value.data : NONE;
}
