import test from "node:test";
import assert from "node:assert/strict";
import { barsToNextPhrase, columnAt, displayGain, fromBandPeaks, parsePhrases, parseThreeBand, phraseAt, sampleThreeBand } from "./three-band.ts";

const payload = (bytes, extra = {}) => ({ source: "rekordbox", columns: bytes.length / 3, columns_per_second: 150, start_ms: -26, duration_ms: bytes.length / 3 / 150 * 1000, scale: 127, data: Buffer.from(bytes).toString("base64"), ...extra });

test("a rekordbox detail wave maps playback time through its timing offset", () => {
  const wave = parseThreeBand(payload(new Array(300 * 3).fill(60)));
  assert.equal(wave.source, "rekordbox");
  assert.equal(columnAt(wave, -26), 0);
  assert.equal(columnAt(wave, 974), 150);
});

test("sampling peaks over a pixel and interpolates when zoomed in", () => {
  const bytes = [];
  for (let c = 0; c < 300; c++) bytes.push(c === 100 ? 120 : 10, 0, 0);
  const wave = parseThreeBand(payload(bytes, { start_ms: 0 }));
  const wide = sampleThreeBand(wave, 0, 2000, 10);
  assert.ok(wide.low[3] > wide.low[2], "the loud column wins its pixel");
  const fine = sampleThreeBand(wave, 660, 20, 40);
  assert.ok(fine.low.every((v) => v >= 0 && v <= 1));
});

test("gain lifts quiet overviews but never shrinks loud ones", () => {
  assert.equal(displayGain(new Uint8Array(30).fill(127), 127), 1);
  assert.ok(displayGain(new Uint8Array(3000).fill(40), 127) > 2.9);
});

test("shared asset band peaks become the same model", () => {
  const wave = fromBandPeaks({ bins_per_second: 100, duration_ms: 1000, amplitude_scale: 255, peaks: [], low: Array(100).fill(255), mid: Array(100).fill(255), high: Array(100).fill(255) });
  assert.equal(wave.columns, 100);
  assert.deepEqual([...wave.data.slice(0, 3)], [127, 108, 64]);
});

test("bars to the next phrase are counted on the deck's beat grid", () => {
  const phrases = parsePhrases({ items: [
    { start_ms: 0, end_ms: 16000, start_beat: 1, end_beat: 33, kind: "intro", label: "Intro 1" },
    { start_ms: 16000, end_ms: 32000, start_beat: 33, end_beat: 65, kind: "chorus", label: "Chorus 1" },
    { start_ms: 1, end_ms: 0, kind: "bogus" },
  ] });
  assert.equal(phrases.length, 2);
  const beatTimesMs = Array.from({ length: 64 }, (_, i) => i * 500);
  assert.equal(phraseAt(phrases, 3000).current.label, "Intro 1");
  assert.equal(barsToNextPhrase(phrases, 3000, { beatTimesMs, beatsPerBar: 4 }), 7);
  assert.equal(barsToNextPhrase(phrases, 15900, { beatTimesMs, beatsPerBar: 4 }), 0);
  assert.equal(barsToNextPhrase(phrases, 3000, { bpm: 120, beatsPerBar: 4 }), 7);
  assert.equal(barsToNextPhrase(phrases, 20000, { beatTimesMs }), null);
});
