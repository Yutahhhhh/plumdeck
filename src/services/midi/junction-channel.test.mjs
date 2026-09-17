import assert from 'node:assert/strict';
import test from 'node:test';
import {controllerTailLock, junctionChannelActive, junctionChannelSettings, junctionChannelTarget, readJunctionChannel} from './junction-channel.ts';

const turn = (overrides = {}) => ({signal: 'onair', nextPeerId: '', outgoingPeerId: '', blocker: {code: '', text: ''}, nextStatus: '', tailDecks: [], repeat: false, outOfQueue: [], incompatiblePeerIds: [], autoFailover: false, ...overrides});
const snapshot = (overrides = {}) => ({active: true, junctionInput: {peerId: 'prev', releasingPeerId: '', receiving: true, latencyMs: 0, underruns: '0', channel: {available: true, volume: .5, eqLow: 1, eqMid: 4, eqHigh: 0, pfl: false}, lane: {bucketMs: 40, peaks: [], decks: []}}, turn: turn(), ...overrides});

test('the J strip maps LEVEL, EQ, assign and CUE with the deck EQ curve', () => {
  assert.equal(readJunctionChannel('D'), 'D');assert.equal(readJunctionChannel('A'), '');assert.equal(readJunctionChannel(null), '');
  assert.deepEqual(junctionChannelSettings('gain', .7, undefined, undefined), {volume: .7});
  assert.deepEqual(junctionChannelSettings('eqMid', .5, undefined, undefined), {eqMid: 1});
  assert.deepEqual(junctionChannelSettings('eqHigh', 1, undefined, undefined), {eqHigh: 4});
  assert.deepEqual(junctionChannelSettings('eqLow', 0, undefined, undefined), {eqLow: 0});
  assert.deepEqual(junctionChannelSettings('assignLeft', 1, true, undefined), {orientation: 0});
  assert.equal(junctionChannelSettings('assignLeft', 0, false, undefined), null, 'release does nothing');
  assert.deepEqual(junctionChannelSettings('pfl', 1, true, {available: true, pfl: true}), {pfl: false});
  assert.equal(junctionChannelSettings('play', 1, true, undefined), null, 'the deck section keeps its deck');
  assert.equal(junctionChannelSettings('trim', .5, undefined, undefined), null, 'J has no trim');
});

test('soft takeover targets the current J values', () => {
  const channel = snapshot().junctionInput.channel;
  assert.equal(junctionChannelTarget('gain', channel), .5);
  assert.equal(junctionChannelTarget('eqMid', channel), 1);
  assert.equal(junctionChannelTarget('eqHigh', channel), 0);
  assert.equal(junctionChannelTarget('eqLow', channel), .5);
  assert.equal(junctionChannelTarget('gain', {available: false}), undefined);
});

test('the strip is live only while this computer has J', () => {
  assert.equal(junctionChannelActive('D', 'D', snapshot()), true);
  assert.equal(junctionChannelActive('D', 'B', snapshot()), false);
  assert.equal(junctionChannelActive('', 'D', snapshot()), false);
  assert.equal(junctionChannelActive('D', 'D', snapshot({junctionInput: undefined})), false, 'without J the channel is its deck again');
});

test('controller gestures on an OUTGOING tail are dropped except loops, and the J strip is never locked', () => {
  const outgoing = snapshot({turn: turn({signal: 'outgoing', tailDecks: ['A']})});
  assert.notEqual(controllerTailLock('play', 'A', outgoing), '');
  assert.notEqual(controllerTailLock('jog', 'A', outgoing), '');
  assert.notEqual(controllerTailLock('gain', 'A', outgoing), '');
  assert.notEqual(controllerTailLock('filterA', undefined, outgoing), '');
  assert.notEqual(controllerTailLock('crossfader', undefined, outgoing), '');
  assert.equal(controllerTailLock('loop', 'A', outgoing), '');
  assert.equal(controllerTailLock('beatLoopPad', 'A', outgoing), '');
  assert.equal(controllerTailLock('pfl', 'A', outgoing), '');
  assert.equal(controllerTailLock('play', 'B', outgoing), '', 'other decks are free');
  assert.equal(controllerTailLock('play', 'A', snapshot()), '', 'nothing is locked while ON AIR');
});
