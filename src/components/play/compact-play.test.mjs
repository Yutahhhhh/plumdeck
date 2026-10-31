import test from 'node:test';
import assert from 'node:assert/strict';
import { compactTiers, compactWavesHeight, nextTempoRange, COMPACT_PADS_HEIGHT, COMPACT_LIBRARY_MIN_HEIGHT, COMPACT_PLAY_MAX_WIDTH, COMPACT_PLAY_QUERY, COMPACT_LIBRARY_EVENT, LIBRARY_EVENT, controllerLibraryEvent, knobLoadDeck, remainingText, stepCursor } from './compact-play.ts';

test('the threshold is one constant and the media query matches below it', () => {
  assert.equal(COMPACT_PLAY_MAX_WIDTH, 900);
  assert.equal(COMPACT_PLAY_QUERY, '(max-width: 899px)');
});

test('only the compact view diverts browse controls; normal Play keeps the library route', () => {
  for (const control of ['browse', 'browsePress', 'load', 'back', 'previous', 'next']) {
    assert.equal(controllerLibraryEvent(control, true), COMPACT_LIBRARY_EVENT);
    assert.equal(controllerLibraryEvent(control, false), LIBRARY_EVENT);
  }
  for (const control of ['zoom', 'padPage', 'tempoRange', 'browseView', 'hotcueMode']) assert.equal(controllerLibraryEvent(control, true), LIBRARY_EVENT);
});

test('the knob cursor is clamped to the loaded rows', () => {
  assert.equal(stepCursor(0, -3, 10), 0);
  assert.equal(stepCursor(8, 5, 10), 9);
  assert.equal(stepCursor(4, 2, 10), 6);
  assert.equal(stepCursor(4, 0, 3), 2);
  assert.equal(stepCursor(3, 1, 0), 0);
});

test('a knob press loads onto the selected deck unless it is playing', () => {
  const visible = ['A', 'B'];
  assert.equal(knobLoadDeck('A', visible, { A: { status: 'paused', track: {} } }), 'A');
  assert.equal(knobLoadDeck('A', visible, { A: { status: 'playing', track: {} }, B: { status: 'paused', track: {} } }), 'B');
  assert.equal(knobLoadDeck('A', ['A', 'B', 'C', 'D'], { A: { status: 'playing', track: {} }, B: { status: 'paused', track: {} }, C: { status: 'empty', track: null } }), 'C');
  assert.equal(knobLoadDeck('C', visible, { A: { status: 'paused', track: {} } }), 'B');
  assert.equal(knobLoadDeck('A', visible, { A: { status: 'playing' }, B: { status: 'playing' } }), null);
});

test('remaining time counts down like a CDJ', () => {
  assert.equal(remainingText(0, 201_000), '-03:21');
  assert.equal(remainingText(200_500, 201_000), '-00:01');
  assert.equal(remainingText(0, 0), '--:--');
});

test('a taller compact window adds the pads, then the library, below capped waveforms', () => {
  const waves = compactWavesHeight(2);
  assert.deepEqual(compactTiers(waves, 2), { pads: false, library: false });
  assert.deepEqual(compactTiers(waves + COMPACT_PADS_HEIGHT, 2), { pads: true, library: false });
  assert.deepEqual(compactTiers(waves + COMPACT_PADS_HEIGHT + COMPACT_LIBRARY_MIN_HEIGHT, 2), { pads: true, library: true });
  // Four decks stack two pad rows under four waveforms.
  assert.deepEqual(compactTiers(compactWavesHeight(4) + COMPACT_PADS_HEIGHT, 4), { pads: false, library: false });
  assert.deepEqual(compactTiers(compactWavesHeight(4) + 2 * COMPACT_PADS_HEIGHT, 4), { pads: true, library: false });
});

test('the tempo range button cycles and wraps', () => {
  assert.equal(nextTempoRange([6, 10, 16, 75], 6), 10);
  assert.equal(nextTempoRange([6, 10, 16, 75], 75), 6);
});
