import type { JunctionSnapshot, JunctionTrack, JunctionTrackRole, JunctionTrackState } from '../../types/junction.ts';

/**
 * A received Junction Live current/next entry. `id` is a temporary UI key
 * derived from the content hash. It is always negative: library Track ids are
 * positive database rows, so a Junction row can never look up, persist to or
 * be mistaken for a local library track.
 */
export type JunctionBrowserTrack = {
  id: number;
  assetId: string;
  title: string;
  artist: string;
  key: string;
  bpm: number | null;
  /** Seconds, like Track.duration. */
  duration: number;
  /** This computer's verified cache file; empty until ready. */
  filepath: string;
  state: JunctionTrackState;
  progress: number;
  detail?: string;
  sourceDjName: string;
  sourceDeck: string;
  onDeck: boolean;
  playing: boolean;
  role: JunctionTrackRole;
  positionMs: number;
  rate: number;
  audibility: number;
  order: number;
};

const ASSET_ID = /^[0-9a-f]{64}$/;

/** 52 bits of the SHA-256 id: a stable, collision-negligible negative safe integer. */
export function junctionTrackUiId(assetId: string): number {
  return -(Number.parseInt(assetId.slice(0, 13), 16) + 1);
}

/** Junction Live is a coordinator-side monitor of a remote performer. */
export function showJunctionTracks(snapshot: JunctionSnapshot | null | undefined): boolean {
  return Boolean(snapshot?.active && snapshot.sessionId
    && snapshot.localPeerId === snapshot.hostPeerId
    && snapshot.performerPeerId && snapshot.performerPeerId !== snapshot.localPeerId
    && Array.isArray(snapshot.junctionTracks));
}

function absolutePath(value: unknown): value is string {
  return typeof value === 'string' && (value.startsWith('/') || /^[A-Za-z]:[\\/]/.test(value) || value.startsWith('\\\\'));
}

function row(track: JunctionTrack): JunctionBrowserTrack | null {
  if (!track || !ASSET_ID.test(track.assetId)) return null;
  const ready = track.state === 'ready' && track.ready === true && absolutePath(track.path);
  return {
    id: junctionTrackUiId(track.assetId),
    assetId: track.assetId,
    title: track.title || 'Junction Live',
    artist: track.artist || '',
    key: track.musicalKey || '',
    bpm: Number.isFinite(track.bpm) && track.bpm > 0 ? track.bpm : null,
    duration: Number.isFinite(track.durationMs) && track.durationMs > 0 ? track.durationMs / 1000 : 0,
    filepath: ready ? track.path! : '',
    state: ready ? 'ready' : track.state === 'ready' ? 'verifying' : track.state,
    progress: Number.isFinite(track.progress) ? Math.min(1, Math.max(0, track.progress)) : 0,
    detail: track.detail,
    sourceDjName: track.sourceDjName || '',
    sourceDeck: track.sourceDeck || '',
    onDeck: Boolean(track.onDeck),
    playing: Boolean(track.playing),
    role: track.role === 'current' ? 'current' : 'next',
    positionMs: Number.isFinite(track.positionMs) ? Math.max(-60_000, Math.min(track.durationMs || 86_400_000, track.positionMs)) : 0,
    rate: Number.isFinite(track.rate) ? Math.max(.25, Math.min(4, track.rate)) : 1,
    audibility: Number.isFinite(track.audibility) ? Math.max(0, Math.min(16, track.audibility)) : 0,
    order: Number.isFinite(track.order) ? track.order : 0,
  };
}

/** The wire carries a pair; the browser exposes it as one Junction Live source. */
export function junctionBrowserTracks(snapshot: JunctionSnapshot | null | undefined): JunctionBrowserTrack[] {
  if (!showJunctionTracks(snapshot)) return [];
  const seen = new Set<string>();
  const rows: JunctionBrowserTrack[] = [];
  for (const track of snapshot!.junctionTracks!) {
    const value = row(track);
    if (!value || seen.has(value.assetId)) continue;
    seen.add(value.assetId);
    rows.push(value);
  }
  return rows.sort((a, b) => Number(a.role === 'next') - Number(b.role === 'next'));
}

export function isJunctionTrackLoadable(track: Pick<JunctionBrowserTrack, 'state' | 'filepath' | 'assetId'> | null | undefined): boolean {
  return Boolean(track && track.state === 'ready' && ASSET_ID.test(track.assetId) && absolutePath(track.filepath));
}

export function junctionTrackStatus(track: Pick<JunctionBrowserTrack, 'state' | 'progress'>): string {
  const percent = `${Math.round(track.progress * 100)}%`;
  switch (track.state) {
    case 'ready': return '準備完了';
    case 'receiving': return `受信中 ${percent}`;
    case 'verifying': return `検証中 ${percent}`;
    case 'failed': return '受信失敗';
    default: return '受信待ち';
  }
}
