import test from 'node:test';
import assert from 'node:assert/strict';
import { isJunctionTrackLoadable, junctionBrowserTracks, junctionTrackUiId, showJunctionTracks } from './tracks.ts';

const asset = (digit) => digit.repeat(64);
const row = (digit, extra = {}) => ({ role: 'current', assetId: asset(digit), title: `Track ${digit}`, artist: 'DJ', musicalKey: '8A', durationMs: 180000, bpm: 128, sizeBytes: 4096, sourcePeerId: 'performer-01', sourceDjName: 'Remote DJ', sourceDeck: 'A', onDeck: true, playing: true, positionMs: 24000, rate: 1, audibility: 1, state: 'receiving', ready: false, order: 1, progress: 0.25, ...extra });
const session = (tracks, extra = {}) => ({ active: true, sessionId: 'session-01', revision: 1, epoch: '2', localPeerId: 'host-0001', hostPeerId: 'host-0001', performerPeerId: 'performer-01', handoffState: 'playing', participants: [], readiness: { ready: false, reasons: [] }, program: { state: 'running' }, connection: { state: 'connected' }, junctionTracks: tracks, ...extra });
const readyRow = (digit, extra = {}) => row(digit, { state: 'ready', ready: true, progress: 1, path: `/Users/dj/Library/Caches/plumdeck/junction/${asset(digit)}`, ...extra });

test('Junction Live is shown only to the coordinator while a remote DJ performs', () => {
  assert.equal(showJunctionTracks(null), false);
  assert.equal(showJunctionTracks({ ...session([]), active: false, sessionId: null }), false);
  assert.equal(showJunctionTracks({ ...session([]), junctionTracks: undefined }), false, 'older native runtimes stay hidden');
  assert.equal(showJunctionTracks(session([])), true, 'the source may wait for its current track');
  assert.equal(showJunctionTracks(session([], { performerPeerId: 'host-0001' })), false, 'local performance restores the real deck display');
  assert.equal(showJunctionTracks(session([], { localPeerId: 'guest-0001' })), false, 'a guest never exposes coordinator cache state');
});

test('temporary UI ids are stable negative safe integers that never address a library row', () => {
  const id = junctionTrackUiId(asset('f'));
  assert(Number.isSafeInteger(id) && id < 0);
  assert.equal(junctionTrackUiId(asset('f')), id);
  assert.notEqual(junctionTrackUiId(asset('a')), junctionTrackUiId(asset('b')));
});

test('the presentation pair is current then next and paths are exposed only after verification', () => {
  const rows = junctionBrowserTracks(session([
    readyRow('a', { role: 'next', sourceDeck: 'B', playing: false, positionMs: 0, audibility: 0 }),
    row('b', { role: 'current' }),
    { ...readyRow('e'), assetId: '../../etc/passwd' },
  ]));
  assert.deepEqual(rows.map(track => track.role), ['current', 'next']);
  assert.equal(isJunctionTrackLoadable(rows[0]), false);
  assert.equal(isJunctionTrackLoadable(rows[1]), true);
  assert.equal(rows[0].positionMs, 24000);
  assert.equal(rows[0].rate, 1);
  const windows = junctionBrowserTracks(session([readyRow('a', { path: `C:/Users/dj/AppData/Local/plumdeck/cache/junction/${asset('a')}` })]));
  assert.equal(isJunctionTrackLoadable(windows[0]), true, 'Windows cache paths are local absolute paths');
});

test('Junction Live data cannot masquerade as a library track', () => {
  const [track] = junctionBrowserTracks(session([readyRow('a')]));
  assert(track.id < 0);
  assert.equal(track.filepath.endsWith(track.assetId), true);
  assert.equal(track.sourceDjName, 'Remote DJ');
});
