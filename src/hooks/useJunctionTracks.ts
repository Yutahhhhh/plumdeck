import { useSyncExternalStore } from 'react';
import type { JunctionSnapshot } from '@/types/junction';
import { junctionState } from '@/services/junction/state';
import { isJunctionTrackLoadable, junctionBrowserTracks, showJunctionTracks, type JunctionBrowserTrack } from '@/services/junction/tracks';

export type JunctionTracksView = { visible: boolean; tracks: JunctionBrowserTrack[]; current: JunctionBrowserTrack | null; next: JunctionBrowserTrack | null; ready: number };
const HIDDEN: JunctionTracksView = { visible: false, tracks: [], current: null, next: null, ready: 0 };
let source: JunctionSnapshot | null = null;
let key = '';
let view = HIDDEN;

// Keep the same view object until the presentation pair actually changes.
function read(): JunctionTracksView {
  const snapshot = junctionState.get();
  if (snapshot === source) return view;
  source = snapshot;
  if (!showJunctionTracks(snapshot)) { key = ''; view = HIDDEN; return view; }
  const tracks = junctionBrowserTracks(snapshot);
  const nextKey = JSON.stringify(tracks);
  if (nextKey !== key || !view.visible) { key = nextKey; view = { visible: true, tracks, current: tracks.find(track => track.role === 'current') ?? null, next: tracks.find(track => track.role === 'next') ?? null, ready: tracks.filter(isJunctionTrackLoadable).length }; }
  return view;
}

export function useJunctionTracks(): JunctionTracksView {
  return useSyncExternalStore(junctionState.subscribe, read, () => HIDDEN);
}
