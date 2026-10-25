import test from 'node:test';
import assert from 'node:assert/strict';
import { AUDITION_SECONDS, auditionStart } from '../audition.ts';

test('an audition starts about 30% into the song', () => {
  assert.equal(auditionStart(300), 90);
  assert.equal(auditionStart(200), 60);
});

test('it starts earlier when less than one audition would remain', () => {
  assert.equal(auditionStart(60), 18);
  assert.equal(auditionStart(45), 13.5);
  assert.equal(auditionStart(40), 10);
  assert.equal(auditionStart(AUDITION_SECONDS), 0);
});

test('short or unknown lengths start from the top', () => {
  assert.equal(auditionStart(12), 0);
  for (const value of [0, -5, null, undefined, NaN, Infinity]) assert.equal(auditionStart(value), 0);
});
