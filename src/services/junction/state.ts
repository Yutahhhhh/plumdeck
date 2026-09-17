import type { JunctionSnapshot } from '../../types/junction.ts';
let snapshot: JunctionSnapshot | null = null;
const listeners = new Set<() => void>();
export const junctionState = {
  get: () => snapshot,
  active: () => Boolean(snapshot?.active && snapshot.sessionId),
  unavailable: () => { if (snapshot?.active) { snapshot = {...snapshot, connection: {state: "unknown", detail: "状態を再確認しています"}}; listeners.forEach(fn => fn()); } },
  subscribe: (fn: () => void) => { listeners.add(fn); return () => { listeners.delete(fn); }; },
  set: (value: JunctionSnapshot) => { snapshot = value; listeners.forEach(fn => fn()); },
};
/** Changes whenever the session, epoch or local DJ changes; queued UI work compares it to drop stale commands. */
export const junctionLeaseKey = () => snapshot?.active && snapshot.sessionId ? `${snapshot.sessionId}:${snapshot.epoch}:${snapshot.localPeerId}` : 'standalone';
