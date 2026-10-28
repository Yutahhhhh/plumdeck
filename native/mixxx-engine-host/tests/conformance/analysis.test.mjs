// Offline checks of the conformance measurements on synthetic signals. No
// host process or audio device: the suite's numbers are only as good as these.
import test from 'node:test';
import assert from 'node:assert/strict';
import { db, dominantFrequency, findCopies, periodByAutocorrelation, residualDb, risingEdges, toneAmplitude, worstOutOfBandDb } from './analysis.mjs';
import { RATE, clickTemplate } from './harness.mjs';

const sine = (hz, seconds, amplitude = 0.25) => Float64Array.from({ length: Math.round(RATE * seconds) }, (_, n) => amplitude * Math.sin(2 * Math.PI * hz * n / RATE));

test('echo copies are located to a fraction of a millisecond', () => {
  const click = clickTemplate(), x = new Float64Array(RATE * 2);
  const at = [0.1, 0.35, 0.6].map(s => Math.round(s * RATE));
  at.forEach((offset, i) => click.forEach((v, k) => { x[offset + k] += v * 0.5 ** i; }));
  const found = findCopies(x, click, { floorDb: -40, minGap: 400 });
  assert.equal(found.length, 3);
  assert(Math.abs((found[1].at - found[0].at) / RATE * 1000 - 250) < 0.05);
  assert(Math.abs(db(found[1].gain / found[0].gain) + 6.02) < 0.1);
});

test('tone amplitude, dominant frequency and loop period', () => {
  assert(Math.abs(db(toneAmplitude(sine(1000, 0.5, 0.5), RATE, 1000)) + 6.02) < 0.05);
  const hz = dominantFrequency(sine(840.896, 0.7), RATE, 700, 1200);
  assert(Math.abs(1200 * Math.log2(hz / 840.896)) < 0.5, `${hz}`);
  const segment = sine(1000, 7.8125 / 1000), looped = new Float64Array(RATE * 0.4);
  for (let i = 0; i < looped.length; ++i) looped[i] = segment[i % segment.length];
  assert(Math.abs(periodByAutocorrelation(looped, RATE, 0.005, 0.012).period * 1000 - 7.8125) < 0.05);
});

test('gate edges, clicks and residuals', () => {
  const gated = sine(1000, 1).map((v, n) => (n / RATE) % 0.125 < 0.0625 ? v : 0.01 * v);
  const { edges } = risingEdges(gated, RATE, { minGapMs: 60 });
  const steps = edges.slice(1).map((e, i) => (e - edges[i]) * 1000);
  assert(steps.every(step => Math.abs(step - 125) < 0.5), JSON.stringify(steps));
  assert(worstOutOfBandDb(sine(1000, 0.5), RATE, 1000) < -60);
  const clicked = sine(1000, 0.5); clicked[RATE / 4] += 0.2;
  assert(worstOutOfBandDb(clicked, RATE, 1000) > -40);
  const dry = sine(1000, 1).map((v, n) => v + 0.1 * Math.sin(n * 0.37));
  assert(residualDb(dry.subarray(1000), dry, RATE * 0.1, RATE * 0.5) < -100);
});
