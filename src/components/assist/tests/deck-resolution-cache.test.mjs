import test from 'node:test';
import assert from 'node:assert/strict';
import { DeckResolutionCache, observedDeckIdentity } from '../deck-resolution-cache.ts';
const deck = { slot: 1, loaded: true, title: 'Bad Girl', artist: 'Usher', tempo_bpm: 88 };
const initial = { decks: [{ slot: 1, track: { id: 1844 } }] };
const deferred = () => { let resolve; const promise = new Promise(done => { resolve = done; }); return { promise, resolve }; };

test('a delayed periodic same-song verification retains the current match throughout', async () => {
  const cache = new DeckResolutionCache();
  cache.observe([deck]); cache.publish(initial);
  const response = deferred();
  cache.observe([{ ...deck, tempo_bpm: 93, display_key: 'C' }]);
  const pending = response.promise.then(value => cache.publish(value));
  assert.strictEqual(cache.value, initial);
  await Promise.resolve();
  assert.strictEqual(cache.value, initial);
  const updated = { ...initial, checked: true };
  response.resolve(updated); await pending;
  assert.strictEqual(cache.value, updated);
});

test('a real track switch hides the old match before its delayed resolution returns', async () => {
  const cache = new DeckResolutionCache();
  cache.observe([deck]); cache.publish(initial);
  const response = deferred();
  cache.observe([{ ...deck, title: 'Think About You', artist: 'Kygo' }]);
  const pending = response.promise.then(value => cache.publish(value));
  assert.equal(cache.value, null);
  response.resolve({ decks: [{ slot: 1, track: { id: 2146 } }] }); await pending;
  assert.equal(cache.value.decks[0].track.id, 2146);
});

test('changing another deck retains the unchanged source resolution while it is rechecked', () => {
  const cache = new DeckResolutionCache();
  const second = { ...deck, slot: 2, title: 'Second', artist: 'Artist' };
  cache.observe([deck, second]);
  cache.publish({ decks: [{ slot: 1, track: { id: 1844 } }, { slot: 2, track: { id: 2002 } }] });

  cache.observe([deck, { ...second, title: 'Changed' }]);

  assert.deepEqual(cache.value?.decks, [{ slot: 1, track: { id: 1844 } }]);
});

test('disconnect/error invalidation cannot revive cached matches on reconnect before verification', () => {
  const cache = new DeckResolutionCache();
  cache.observe([deck]); cache.publish(initial);
  cache.clear();
  assert.equal(cache.value, null);
  cache.observe([deck]);
  assert.equal(cache.value, null);
});

test('slot order and pitch do not change identity, but unloading or changing artist does', () => {
  const second = { ...deck, slot: 2, title: 'Second' };
  assert.equal(observedDeckIdentity([deck, second]), observedDeckIdentity([second, { ...deck, tempo_bpm: 100 }]));
  assert.notEqual(observedDeckIdentity([deck]), observedDeckIdentity([{ ...deck, loaded: false }]));
  assert.notEqual(observedDeckIdentity([deck]), observedDeckIdentity([{ ...deck, artist: 'Other' }]));
});

test('the advancing elapsed readout never changes identity, so decks are not re-resolved every second', () => {
  assert.equal(
    observedDeckIdentity([{ ...deck, elapsed_seconds: 12.3 }]),
    observedDeckIdentity([{ ...deck, elapsed_seconds: 13.4 }]),
  );
  const cache = new DeckResolutionCache();
  cache.observe([{ ...deck, elapsed_seconds: 1 }]); cache.publish(initial);
  cache.observe([{ ...deck, elapsed_seconds: 2 }]);
  assert.strictEqual(cache.value, initial);
});
