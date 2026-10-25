import test from 'node:test';
import assert from 'node:assert/strict';
import { advancePlayback, EMPTY_PLAYBACK, pickMaster, playbackReadable, playingSlots } from '../deck-playback.ts';

const deck = (slot, elapsed, extra = {}) => ({ slot, loaded: true, title: `Track ${slot}`, artist: 'Artist', elapsed_seconds: elapsed, ...extra });
/** Feeds snapshots one second apart; each entry is the list of decks at that tick. */
const run = (ticks, start = EMPTY_PLAYBACK, t0 = 1_000_000) => ticks.reduce((state, decks, i) => advancePlayback(state, t0 + i * 1000, decks), start);

test('the first snapshot never claims playing; the second one that advances does', () => {
  const first = run([[deck(1, 10)]]);
  assert.deepEqual(playingSlots(first), []);
  assert.deepEqual(playingSlots(run([[deck(1, 10)], [deck(1, 11.1)]])), [1]);
});

test('a deck stops only after two samples without motion', () => {
  const playing = run([[deck(1, 10)], [deck(1, 11)], [deck(1, 12)]]);
  assert.deepEqual(playingSlots(playing), [1]);
  const once = advancePlayback(playing, 1_003_000, [deck(1, 12)]);
  assert.deepEqual(playingSlots(once), [1]);
  const twice = advancePlayback(once, 1_004_000, [deck(1, 12)]);
  assert.deepEqual(playingSlots(twice), []);
  assert.deepEqual(playingSlots(advancePlayback(twice, 1_005_000, [deck(1, 13)])), [1]);
});

test('one missed or repeated reading does not flap a playing deck', () => {
  const playing = run([[deck(1, 10)], [deck(1, 11)]]);
  const jittered = advancePlayback(playing, 1_002_000, [deck(1, 11)]);
  assert.deepEqual(playingSlots(advancePlayback(jittered, 1_003_000, [deck(1, 12)])), [1]);
  const missed = advancePlayback(playing, 1_002_000, [deck(1, null)]);
  assert.deepEqual(playingSlots(missed), [1]);
  assert.deepEqual(playingSlots(advancePlayback(missed, 1_003_000, [deck(1, 13)])), [1]);
});

test('samples closer than the minimum gap are ignored so cached snapshots cannot stop a deck', () => {
  const playing = run([[deck(1, 10)], [deck(1, 11)]]);
  assert.strictEqual(advancePlayback(playing, 1_001_100, [deck(1, 11)]), playing);
  assert.strictEqual(advancePlayback(playing, 1_001_000, [deck(1, 11)]), playing);
  assert.strictEqual(advancePlayback(playing, 1_000_500, [deck(1, 11)]), playing);
  const later = advancePlayback(advancePlayback(playing, 1_001_100, [deck(1, 11)]), 1_002_000, [deck(1, 12)]);
  assert.deepEqual(playingSlots(later), [1]);
});

test('a seek on a paused deck is not playing, and a seek on a playing deck keeps playing', () => {
  const paused = run([[deck(1, 10)], [deck(1, 10)]]);
  assert.deepEqual(playingSlots(advancePlayback(paused, 1_002_000, [deck(1, 95)])), []);
  const playing = run([[deck(1, 10)], [deck(1, 11)]]);
  const seeked = advancePlayback(playing, 1_002_000, [deck(1, 95)]);
  assert.deepEqual(playingSlots(seeked), [1]);
  assert.deepEqual(playingSlots(advancePlayback(seeked, 1_003_000, [deck(1, 96)])), [1]);
});

test('a loop wrap does not stop a deck, but a cue return that stays put does', () => {
  const playing = run([[deck(1, 40)], [deck(1, 41)], [deck(1, 42)]]);
  const wrapped = advancePlayback(playing, 1_003_000, [deck(1, 38)]);
  assert.deepEqual(playingSlots(wrapped), [1]);
  assert.deepEqual(playingSlots(advancePlayback(wrapped, 1_004_000, [deck(1, 39)])), [1]);
  const cued = advancePlayback(wrapped, 1_004_000, [deck(1, 38)]);
  assert.deepEqual(playingSlots(cued), []);
});

test('loading another song, unloading, or losing the decks resets the deck', () => {
  const playing = run([[deck(1, 10)], [deck(1, 11)]]);
  const reloaded = advancePlayback(playing, 1_002_000, [deck(1, 12, { title: 'Other' })]);
  assert.deepEqual(playingSlots(reloaded), []);
  assert.deepEqual(playingSlots(advancePlayback(playing, 1_002_000, [deck(1, 12, { loaded: false })])), []);
  assert.equal(advancePlayback(playing, 1_002_000, []).slots.size, 0);
});

test('an unreadable elapsed readout never plays and stops a deck that was playing', () => {
  const unreadable = run([[deck(1, null)], [deck(1, null)], [deck(1, null)]]);
  assert.deepEqual(playingSlots(unreadable), []);
  assert.equal(playbackReadable([deck(1, null), deck(2, null)]), false);
  assert.equal(playbackReadable([deck(1, null), deck(2, 3)]), true);
  assert.equal(playbackReadable([deck(1, null), deck(2, 3, { loaded: false })]), false);
  const playing = run([[deck(1, 10)], [deck(1, 11)]]);
  const lost = run([[deck(1, null)], [deck(1, null)]], playing, 1_002_000);
  assert.deepEqual(playingSlots(lost), []);
});

test('a slow-pitched deck still reads as playing', () => {
  assert.deepEqual(playingSlots(run([[deck(1, 10)], [deck(1, 10.84)]])), [1]);
  assert.deepEqual(playingSlots(run([[deck(1, 10)], [deck(1, 11.5)]])), [1]);
});

test('MASTER: one playing deck is followed, none keeps the last', () => {
  const state = run([[deck(1, 10), deck(2, 5)], [deck(1, 10), deck(2, 6)]]);
  assert.equal(pickMaster(state, null, 1), 2);
  assert.equal(pickMaster(state, 1, 1), 2);
  const stopped = run([[deck(1, 10), deck(2, 5)], [deck(1, 10), deck(2, 5)], [deck(1, 10), deck(2, 5)]]);
  assert.equal(pickMaster(stopped, 2, 2), 2);
  assert.equal(pickMaster(stopped, null, 1), 1);
  assert.equal(pickMaster(EMPTY_PLAYBACK, null, 4), 4);
});

test('MASTER: with several playing it stays on the followed deck until that one stops', () => {
  const both = run([[deck(1, 60), deck(2, 0)], [deck(1, 61), deck(2, 1)], [deck(1, 62), deck(2, 2)]]);
  assert.deepEqual(playingSlots(both), [1, 2]);
  assert.equal(pickMaster(both, 2, 2), 2);
  assert.equal(pickMaster(both, 1, 1), 1);
  const oneStops = run([[deck(1, 62), deck(2, 3)], [deck(1, 62), deck(2, 4)]], both, 1_003_000);
  assert.deepEqual(playingSlots(oneStops), [2]);
  assert.equal(pickMaster(oneStops, 1, 1), 2);
});

test('MASTER: choosing it with several playing prefers the longest-playing, then the furthest along', () => {
  const longer = run([[deck(1, 60)], [deck(1, 61)], [deck(1, 62), deck(2, 0)], [deck(1, 63), deck(2, 1)]]);
  assert.deepEqual(playingSlots(longer), [1, 2]);
  assert.equal(pickMaster(longer, null, 2), 1);
  const together = run([[deck(1, 5), deck(2, 90)], [deck(1, 6), deck(2, 91)]]);
  assert.equal(pickMaster(together, null, 1), 2);
  const tie = run([[deck(1, 5), deck(2, 5)], [deck(1, 6), deck(2, 6)]]);
  assert.equal(pickMaster(tie, null, 2), 1);
});
