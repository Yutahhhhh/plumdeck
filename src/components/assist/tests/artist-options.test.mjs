import test from 'node:test';
import assert from 'node:assert/strict';
import { addArtist, foldArtist, matchArtists, MAX_ARTISTS } from '../artist-options.ts';
import { parseTempoTolerance } from '../tempo-tolerance.ts';

const library = [
  { name: 'Drake', tracks: 40 },
  { name: 'Calvin Harris', tracks: 30 },
  { name: 'Lil Drake', tracks: 3 },
  { name: 'Ｄａｆｔ Ｐｕｎｋ', tracks: 12 },
];

test('typed text matches names that start with it before names that contain it', () => {
  assert.deepEqual(matchArtists(library, 'dra', []).map(item => item.name), ['Drake', 'Lil Drake']);
});

test('width and case fold like the backend filter', () => {
  assert.equal(foldArtist('Ｄａｆｔ  Ｐｕｎｋ'), 'daft punk');
  assert.deepEqual(matchArtists(library, 'daft', []).map(item => item.name), ['Ｄａｆｔ Ｐｕｎｋ']);
});

test('an empty query keeps library order and selected artists are not offered again', () => {
  assert.deepEqual(matchArtists(library, '', ['drake']).map(item => item.name), ['Calvin Harris', 'Lil Drake', 'Ｄａｆｔ Ｐｕｎｋ']);
  assert.equal(matchArtists(library, '', [], 2).length, 2);
});

test('adding an artist ignores blanks and spellings already selected', () => {
  const selected = ['Drake'];
  assert.equal(addArtist(selected, '  '), selected);
  assert.equal(addArtist(selected, 'DRAKE'), selected);
  assert.deepEqual(addArtist(selected, ' Calvin   Harris '), ['Drake', 'Calvin Harris']);
  const full = Array.from({ length: MAX_ARTISTS }, (_, index) => `Artist ${index}`);
  assert.equal(addArtist(full, 'One More'), full);
});

test('only offered tempo tolerances are restored from storage', () => {
  assert.equal(parseTempoTolerance('3'), 3);
  assert.equal(parseTempoTolerance(null), null);
  assert.equal(parseTempoTolerance('5'), null);
  assert.equal(parseTempoTolerance('abc'), null);
});
