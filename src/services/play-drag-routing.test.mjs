import test from 'node:test';
import assert from 'node:assert/strict';
import { readPlayTrackDrag, routePlayTrackDrop } from './play-drag-routing.ts';

const track = { id: 7, filepath: 'C:\\Music\\track.mp3', duration: 180, title: 'Track' };
const active = { kind: 'play-track', track };

test('validates internal Play track drags without relying on DataTransfer', () => {
  assert.deepEqual(readPlayTrackDrag(active), track);
  for (const value of [null, {}, { kind: 'other', track }, { kind: 'play-track', track: { ...track, id: 0 } }, { kind: 'play-track', track: { ...track, filepath: '' } }]) assert.equal(readPlayTrackDrag(value), null);
});

test('routes every deck explicitly and never falls back to another deck', () => {
  const loads = [];
  for (const deck of ['A', 'B', 'C', 'D']) assert.equal(routePlayTrackDrop(active, { kind: 'deck', deck }, (id, value) => loads.push([id, value.id])), true);
  assert.deepEqual(loads, [['A', 7], ['B', 7], ['C', 7], ['D', 7]]);
  assert.equal(routePlayTrackDrop(active, { kind: 'deck', deck: 'E' }, () => assert.fail()), false);
  assert.equal(routePlayTrackDrop(active, null, () => assert.fail()), false);
});

test('routes playlist and sampler action targets once', () => {
  const accepted = [];
  assert.equal(routePlayTrackDrop(active, { kind: 'action', accept: value => accepted.push(value.id) }, () => assert.fail()), true);
  assert.deepEqual(accepted, [7]);
});
