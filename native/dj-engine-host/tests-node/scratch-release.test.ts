import assert from "node:assert/strict";
import test from "node:test";
import { classifyRelease, MotionHistory, type MotionSample } from "../../../src/services/dj-engine/scratch-release.ts";
import { Backspin, BACKSPIN_MOTION } from "../../../src/services/dj-engine/backspin.ts";

/** velocities は 8ms ごとの速度（通常速度比）。最初のサンプルは t=0, 変位 0。 */
function stroke(velocities: number[], stepMs = 8): MotionSample[] {
  const samples: MotionSample[] = [{ time: 0, positionMs: 0 }];
  for (const velocity of velocities) {
    const last = samples[samples.length - 1];
    samples.push({ time: last.time + stepMs, positionMs: last.positionMs + velocity * stepMs });
  }
  return samples;
}
const releaseAt = (samples: MotionSample[], delay = 2) => samples[samples.length - 1].time + delay;

test("a sustained fast reverse throw released while moving is a backspin", () => {
  const samples = stroke(Array(12).fill(-4));
  const decision = classifyRelease(samples, releaseAt(samples), true);
  assert.equal(decision.kind, "backspin");
  assert.ok(decision.kind === "backspin" && Math.abs(decision.velocity + 4) < 1e-9);
});

test("paused decks never backspin so a quick cue drag lands where it was dropped", () => {
  const samples = stroke(Array(12).fill(-6));
  assert.deepEqual(classifyRelease(samples, releaseAt(samples), false), { kind: "scratch" });
});

test("scratch releases are recognized without waiting", () => {
  const throwBack = stroke(Array(12).fill(-4));
  // 手を止めてから離した。
  assert.equal(classifyRelease(throwBack, releaseAt(throwBack, 40), true).kind, "scratch");
  // 引きが短い（スクラッチの小刻みな引き）。
  const short = stroke([3, 3, 3, -4, -4, -4]);
  assert.equal(classifyRelease(short, releaseAt(short), true).kind, "scratch");
  // 折り返し直前で減速している。
  const slowing = stroke([-6, -6, -6, -6, -6, -6, -4, -2, -1.2, -1.2]);
  assert.equal(classifyRelease(slowing, releaseAt(slowing), true).kind, "scratch");
  // 前向きの投げと、ゆっくりした逆回転。
  const forward = stroke(Array(12).fill(6));
  assert.equal(classifyRelease(forward, releaseAt(forward), true).kind, "scratch");
  const gentle = stroke(Array(12).fill(-1));
  assert.equal(classifyRelease(gentle, releaseAt(gentle), true).kind, "scratch");
  assert.equal(classifyRelease([], 0, true).kind, "scratch");
});

test("a reverse stroke after forward motion only measures the reverse part", () => {
  const samples = stroke([5, 5, 5, 5, ...Array(9).fill(-5)]);
  assert.equal(classifyRelease(samples, releaseAt(samples), true).kind, "backspin");
});

test("release velocity is capped at the engine scratch authority", () => {
  const samples = stroke(Array(12).fill(-40));
  const decision = classifyRelease(samples, releaseAt(samples), true);
  assert.ok(decision.kind === "backspin" && decision.velocity === -16);
});

test("motion history stays bounded and monotonic", () => {
  const history = new MotionHistory();
  for (let index = 0; index < 400; index++) history.push(index * 4, -index);
  history.push(100, -401);
  assert.ok(history.samples.length <= 128);
  assert.ok(history.samples[0].time >= 1600 - 500 - 4);
  for (let index = 1; index < history.samples.length; index++) assert.ok(history.samples[index].time >= history.samples[index - 1].time);
});

function fakeTimers() {
  let now = 0;
  let callback: (() => void) | null = null;
  return {
    timers: {
      now: () => now,
      every: (next: () => void) => { callback = next; return 1; },
      cancel: () => { callback = null; },
    },
    advance(ms: number, step: number = BACKSPIN_MOTION.tickMs) {
      for (let elapsed = 0; elapsed < ms && callback; elapsed += step) { now += step; callback(); }
    },
    jump(ms: number) { now += ms; callback?.(); },
    get running() { return callback !== null; },
    get now() { return now; },
  };
}

function spin(weight: number, velocity = -8) {
  const clock = fakeTimers();
  const moves: number[] = [];
  const lands: { position: number; at: number }[] = [];
  const backspin = new Backspin({ velocity, weight, positionMs: -300, timers: clock.timers,
    move: (position) => { assert.equal(lands.length, 0, "no motion after landing"); moves.push(position); },
    land: (position) => { lands.push({ position, at: clock.now }); } });
  return { clock, moves, lands, backspin };
}

test("JOG weight changes how long and how far a backspin spins, then lands once", () => {
  const light = spin(0), heavy = spin(1), medium = spin(0.5);
  for (const run of [light, heavy, medium]) run.clock.advance(5000);
  for (const run of [light, heavy, medium]) {
    assert.equal(run.lands.length, 1);
    assert.equal(run.backspin.active, false);
    assert.equal(run.clock.running, false);
    for (let index = 1; index < run.moves.length; index++) assert.ok(run.moves[index] < run.moves[index - 1], "keeps spinning backward");
  }
  assert.ok(light.lands[0].at > medium.lands[0].at && medium.lands[0].at > heavy.lands[0].at);
  assert.ok(light.lands[0].position < medium.lands[0].position && medium.lands[0].position < heavy.lands[0].position);
  assert.ok(heavy.lands[0].at < 300, `HEAVY brakes hard: ${heavy.lands[0].at}ms`);
  assert.ok(light.lands[0].at > 1000 && light.lands[0].at < 2500, `LIGHT coasts: ${light.lands[0].at}ms`);
});

test("grabbing a spinning platter keeps the gesture without landing", () => {
  const run = spin(0);
  run.clock.advance(200);
  const position = run.backspin.grab();
  assert.equal(position, run.moves[run.moves.length - 1]);
  run.clock.advance(1000);
  assert.equal(run.lands.length, 0);
  assert.equal(run.backspin.stop(), undefined, "a grabbed spin cannot land later");
  assert.equal(run.lands.length, 0);
});

test("stop lands immediately and only once", () => {
  const run = spin(0.5);
  run.clock.advance(50);
  run.backspin.stop(); run.backspin.stop();
  run.clock.advance(1000);
  assert.equal(run.lands.length, 1);
});

test("a stalled timer lands instead of jumping through the skipped time", () => {
  const run = spin(0);
  run.clock.advance(50);
  const before = run.moves[run.moves.length - 1];
  run.clock.jump(BACKSPIN_MOTION.maxGapMs + 1);
  assert.deepEqual(run.lands.map(item => item.position), [before]);
});

test("a release already slower than the landing speed lands at once", () => {
  const run = spin(0, -BACKSPIN_MOTION.landSpeed / 2);
  assert.equal(run.lands.length, 1);
  assert.equal(run.moves.length, 0);
  assert.equal(run.clock.running, false);
});
