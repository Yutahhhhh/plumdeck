import assert from "node:assert/strict";
import test from "node:test";
import { estimateLag } from "./align.ts";
import { ProgramAssembler } from "./program.ts";
import { ElasticBuffer } from "./elastic.ts";

const SR = 48000;

/** Deterministic PRNG (mulberry32). */
function rng(seed) {
  return () => {
    seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/**
 * Music-like test signal: a kick every beat, a bass line, chords and hats.
 * Band-limited enough that cubic interpolation is accurate, rich enough to
 * have one unambiguous correlation peak per beat grid ambiguity.
 */
function track(seconds, seed, bpm = 124) {
  const n = Math.round(seconds * SR);
  const out = new Float32Array(n);
  const r = rng(seed);
  const notes = Array.from({ length: 64 }, () => 55 * 2 ** (Math.floor(r() * 24) / 12));
  const beat = (60 / bpm) * SR;
  let noise = 0;
  for (let i = 0; i < n; i += 1) {
    const b = Math.floor(i / beat);
    const tb = (i - b * beat) / SR;
    const kick = Math.sin(2 * Math.PI * (50 + 80 * Math.exp(-tb * 30)) * tb) * Math.exp(-tb * 8);
    const f = notes[b % notes.length];
    const bass = 0.3 * Math.sin((2 * Math.PI * f * i) / SR);
    const chord = 0.15 * Math.sin((2 * Math.PI * f * 4 * i) / SR) + 0.1 * Math.sin((2 * Math.PI * f * 5 * i) / SR);
    noise = 0.7 * noise + 0.3 * (r() * 2 - 1);
    const hat = 0.2 * noise * Math.exp(-(((i + beat / 2) % beat) / SR) * 40);
    out[i] = 0.5 * kick + bass + chord + hat;
  }
  return out;
}

const delayed = (signal, lag, gain = 1) => {
  const out = new Float32Array(signal.length);
  for (let i = lag; i < signal.length; i += 1) out[i] = gain * signal[i - lag];
  return out;
};
const mixOf = (...parts) => {
  const out = new Float32Array(parts[0].length);
  for (const p of parts) for (let i = 0; i < out.length; i += 1) out[i] += p[i];
  return out;
};

test("estimateLag recovers the loop delay to the sample, with the next DJ's track equally loud", () => {
  const a = track(8, 1);
  const own = track(8, 2, 126);
  for (const lag of [3301, 14400, 27931, 61234]) {
    const mix = mixOf(delayed(a, lag, 0.8), own);
    const est = estimateLag(a, mix, { minLag: 0, maxLag: 96000, window: 4 * SR });
    assert.ok(est, `lag ${lag}`);
    assert.equal(est.lag, lag);
    assert.ok(est.confidence > 0.4, `confidence ${est.confidence}`);
  }
});

test("estimateLag reports low confidence when the J deck is faded out", () => {
  const a = track(8, 1);
  const own = track(8, 2, 126);
  const est = estimateLag(a, own, { minLag: 0, maxLag: 96000, window: 4 * SR });
  assert.ok(!est || est.confidence < 0.2, `confidence ${est?.confidence}`);
});

/**
 * Runs a host session: stream A is on air, stream B is A delayed by `lag`
 * (as the next DJ heard it) plus B's own decks. Feeds both in 128-frame
 * quanta exactly like the capture worklet, presses NEXT at `nextAt` seconds
 * and returns the rendered program.
 */
function runSession({ lag, seconds, nextAt, ownGain = 0, ownStartsAt = 0, delaySeconds = 3, jGain = 1, prior = null }) {
  const a = track(seconds, 7);
  const own = track(seconds, 8, 126);
  const b = new Float32Array(a.length);
  for (let i = 0; i < b.length; i += 1) {
    b[i] = (i >= lag ? jGain * a[i - lag] : 0) + (i >= ownStartsAt * SR ? ownGain * own[i] : 0);
  }
  const program = new ProgramAssembler({ delaySeconds });
  const out = new Float32Array(a.length);
  const q = 128;
  let plan = null;
  program.ingest(0, 0, { A: undefined, B: undefined });
  program.setSource("A");
  for (let f = 0; f + q <= a.length; f += q) {
    program.ingest(f, q, {
      A: { left: a.subarray(f, f + q), right: a.subarray(f, f + q) },
      B: { left: b.subarray(f, f + q), right: b.subarray(f, f + q) },
    });
    if (!plan && f >= nextAt * SR) plan = program.handoff("B", prior);
    const block = program.render(q);
    out.set(block.left, f);
  }
  return { a, b, own, out, plan, program };
}

test("handoff splices A into B's mix with no repeat and no skip of A's audio", () => {
  const lag = 21_337; // ~445 ms loop
  const { a, out, plan, program } = runSession({ lag, seconds: 14, nextAt: 8 });
  assert.equal(plan.status, "aligned");
  assert.equal(plan.lagFrames, lag);
  const D = 3 * SR;
  // Program sample k plays A's frame k − D before the splice, and (because B
  // carries A delayed by exactly lag) still A's frame k − D after it, until
  // the delay-recovery loop starts to slow the reader down.
  const spliceOut = plan.at + D; // output index where the crossfade starts
  let worst = 0;
  for (let k = D + 2000; k < spliceOut + SR / 2; k += 1) worst = Math.max(worst, Math.abs(out[k] - a[k - D]));
  assert.ok(worst < 1e-3, `max deviation from A ${worst}`);
  assert.equal(program.source, "B");
  assert.ok(program.delay < D, "delay was spent");
});

test("after the handoff the next DJ's own decks come in and A's tail is not doubled", () => {
  const lag = 9_600;
  const { a, own, out, plan } = runSession({ lag, seconds: 12, nextAt: 7, ownGain: 0.5, ownStartsAt: 9 });
  assert.equal(plan.status, "aligned");
  const D = 3 * SR;
  // Well after the crossfade, program = A content + B's own track, each once.
  // Compare at the reader's actual position over a short window where the
  // rate is still ~1 (the recovery slew is slow).
  const k0 = plan.at + D + 2 * 1440;
  let worst = 0;
  for (let k = k0; k < k0 + 4800; k += 1) {
    const src = k - D; // A frame being played
    const expected = a[src] + (src + lag >= 9 * SR ? 0.5 * own[src + lag] : 0);
    worst = Math.max(worst, Math.abs(out[k] - expected));
  }
  assert.ok(worst < 2e-3, `deviation ${worst}`);
});

test("delay recovers to D after a handoff and the rate returns to exactly 1", () => {
  const program = new ProgramAssembler({ delaySeconds: 3 });
  const q = 1024;
  const silence = new Float32Array(q);
  program.ingest(0, 0, { A: undefined, B: undefined });
  program.setSource("A");
  for (let f = 0; f < 400 * SR; f += q) {
    program.ingest(f, q, { A: { left: silence, right: silence }, B: { left: silence, right: silence } });
    if (f === 6 * SR) assert.equal(program.handoff("B", 24_000).lagFrames, 24_000);
    program.render(q);
  }
  assert.ok(Math.abs(program.delay - 3 * SR) <= 480, `delay error ${program.delay - 3 * SR}`);
  assert.equal(program.rate, 1);
});

test("estimateLag survives codec-like damage (lowpass, noise)", () => {
  const a = track(8, 3);
  const own = track(8, 4, 128);
  const r = rng(99);
  const lag = 17_777;
  const mix = new Float32Array(a.length);
  let lp = 0;
  for (let i = lag; i < a.length; i += 1) {
    lp += 0.5 * (a[i - lag] - lp); // ~6 kHz one-pole lowpass
    mix[i] = 0.9 * lp + 0.6 * own[i] + 0.02 * (r() * 2 - 1);
  }
  const est = estimateLag(a, mix, { minLag: 0, maxLag: 96000, window: 4 * SR });
  assert.ok(est && Math.abs(est.lag - lag) <= 1, `lag ${est?.lag}`);
  assert.ok(est.confidence > 0.2);
});

test("rate never deviates more than 0.2 % and never steps", () => {
  const program = new ProgramAssembler({ delaySeconds: 3 });
  const q = 128;
  const silence = new Float32Array(q);
  let last = 1;
  let maxStep = 0;
  let maxDev = 0;
  program.ingest(0, 0, { A: undefined, B: undefined });
  program.setSource("A");
  for (let f = 0; f < 200 * SR; f += q) {
    program.ingest(f, q, { A: { left: silence, right: silence }, B: { left: silence, right: silence } });
    if (f === 6 * SR) program.handoff("B", 30_000);
    program.render(q);
    maxStep = Math.max(maxStep, Math.abs(program.rate - last));
    maxDev = Math.max(maxDev, Math.abs(program.rate - 1));
    last = program.rate;
  }
  assert.ok(maxDev <= 0.002 + 1e-12, `max deviation ${maxDev}`);
  assert.ok(maxStep < 2e-6, `max step ${maxStep}`);
});

test("faded J deck: splice is reported unaligned and uses the timestamp prior", () => {
  const { plan } = runSession({ lag: 20_000, seconds: 12, nextAt: 8, jGain: 0, ownGain: 0.8, prior: 19_000 });
  assert.equal(plan.status, "unaligned");
  assert.equal(plan.lagFrames, 19_000);
});

test("a loop longer than the held delay is reported late and clamped", () => {
  const { plan, program } = runSession({ lag: 60_000, seconds: 12, nextAt: 8, delaySeconds: 1.2, prior: 60_000 });
  assert.equal(plan.status, "late");
  assert.ok(plan.lagFrames < 60_000);
  assert.equal(program.source, "B");
});

test("prior narrows the search but the audio decides", () => {
  const { plan } = runSession({ lag: 30_011, seconds: 12, nextAt: 8, prior: 28_000, ownGain: 0.7, ownStartsAt: 0 });
  assert.equal(plan.status, "aligned");
  assert.equal(plan.lagFrames, 30_011);
});

test("several handoffs in a row keep the A→B→C content continuous", () => {
  const seconds = 30;
  const a = track(seconds, 11);
  const lagB = 14_000, lagC = 17_500;
  const b = delayed(a, lagB);
  const c = delayed(b, lagC); // C heard B (which carried A)
  const program = new ProgramAssembler({ delaySeconds: 3 });
  const out = new Float32Array(a.length);
  const q = 128;
  program.ingest(0, 0, { A: undefined });
  program.setSource("A");
  const plans = [];
  for (let f = 0; f + q <= a.length; f += q) {
    const blk = (s) => ({ left: s.subarray(f, f + q), right: s.subarray(f, f + q) });
    program.ingest(f, q, { A: blk(a), B: blk(b), C: blk(c) });
    if (f === 8 * SR) plans.push(program.handoff("B"));
    if (f === 9 * SR) plans.push(program.handoff("C"));
    out.set(program.render(q).left, f);
  }
  assert.deepEqual(plans.map((p) => [p.status, p.lagFrames]), [["aligned", lagB], ["aligned", lagC]]);
  // The program is a continuous rendition of A. Map each output sample back to
  // A by correlation-free comparison: find the A index that matches a probe
  // window after both splices, then check continuity across them.
  const D = 3 * SR;
  let worst = 0;
  for (let k = D + 1000; k < plans[1].at + D + SR / 4; k += 1) worst = Math.max(worst, Math.abs(out[k] - a[k - D]));
  assert.ok(worst < 2e-3, `deviation ${worst}`);
});

test("elastic buffer tracks a drifting producer without under- or overruns", () => {
  const buffer = new ElasticBuffer({ targetFill: 3072 });
  const producerRate = 1.0001; // 100 ppm fast
  const chunk = 441;
  let produced = 0;
  let phase = 0;
  const outL = new Float32Array(128), outR = new Float32Array(128);
  for (let quantum = 0; quantum < (600 * SR) / 128; quantum += 1) {
    // Producer delivers in bursty chunks.
    phase += 128 * producerRate;
    while (phase - produced >= chunk) {
      const block = new Float32Array(chunk).fill(0.25);
      buffer.push(block, block);
      produced += chunk;
    }
    buffer.pull(outL, outR);
  }
  assert.equal(buffer.underruns, 0);
  assert.equal(buffer.skips, 0);
  assert.ok(Math.abs(buffer.fill - 3072) < 1200, `fill ${buffer.fill}`);
});
