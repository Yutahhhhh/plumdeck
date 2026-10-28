import assert from "node:assert/strict";
import test from "node:test";
import { pressFx, releaseFx, releaseAllFx, enqueuePadFx } from "./pad-fx-stack.ts";
import { registerHooks } from "node:module";
// No native engine, audio, or Junction service is involved in these tests.
registerHooks({ resolve(specifier, context, nextResolve) {
  if (specifier === "../junction/client.ts" && context.parentURL?.endsWith("/ddj1000-runtime.ts")) {
    return { url: "data:text/javascript,export%20async%20function%20junctionCommand()%20%7B%20throw%20new%20Error('unexpected%20Junction%20call')%20%7D", shortCircuit: true };
  }
  return nextResolve(specifier, context);
} });
const { Ddj1000Runtime } = await import("../../services/midi/ddj1000-runtime.ts");
import { selectPads } from "../../services/midi/pad-state.ts";

const same = (a, b) => a === b;
const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };

test("failed replacement leaves the acknowledged effect available for release", async () => {
  const state = { pads: [], sounding: null }, calls = [];
  const send = async (pad, enabled) => { calls.push([pad, enabled]); if (pad === "B" && enabled) throw Error("rejected"); };
  await pressFx(state, "A", same, send);
  await assert.rejects(pressFx(state, "B", same, send));
  await releaseFx(state, "A", same, send);
  await releaseFx(state, "B", same, send);
  assert.deepEqual(calls, [["A", true], ["B", true], ["A", true], ["A", false]]);
  assert.deepEqual(state, { pads: [], sounding: null });
});

test("failed replacement restores a previous held pad after best-effort cleanup", async () => {
  const state = { pads: [], sounding: null }, calls = [];
  let engine = null;
  const send = async (pad, enabled) => {
    calls.push([pad, enabled]);
    if (pad === "B" && enabled) { engine = null; throw Error("reply lost"); }
    engine = enabled ? pad : null;
  };
  await pressFx(state, "A", same, send);
  await assert.rejects(pressFx(state, "B", same, send));
  assert.equal(engine, "A");
  await releaseFx(state, "A", same, send);
  assert.equal(engine, null);
});

test("failed fallback retains the effect that still needs an OFF", async () => {
  const state = { pads: [], sounding: null }, calls = [];
  let aPresses = 0;
  const send = async (pad, enabled) => { calls.push([pad, enabled]); if (pad === "A" && enabled && ++aPresses === 2) throw Error("rejected"); };
  await pressFx(state, "A", same, send);
  await pressFx(state, "B", same, send);
  await assert.rejects(releaseFx(state, "B", same, send));
  await releaseFx(state, "A", same, send);
  assert.deepEqual(calls, [["A", true], ["B", true], ["A", true], ["B", false]]);
});

test("UI queue preserves press/release ordering during slow and failed replies", async () => {
  const state = { pads: [], sounding: null }, queue = { tail: Promise.resolve() }, calls = [], gate = deferred();
  const send = async (pad, enabled) => { calls.push([pad, enabled]); if (pad === "A" && enabled) await gate.promise; if (pad === "B" && enabled) throw Error("rejected"); };
  enqueuePadFx(queue, () => pressFx(state, "A", same, send));
  enqueuePadFx(queue, () => pressFx(state, "B", same, send));
  enqueuePadFx(queue, () => releaseFx(state, "A", same, send));
  enqueuePadFx(queue, () => releaseFx(state, "B", same, send));
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(calls, [["A", true]]);
  gate.resolve(); await queue.tail;
  assert.deepEqual(calls, [["A", true], ["B", true], ["A", true], ["A", false]]);
  assert.deepEqual(state, { pads: [], sounding: null });
});

test("a failed OFF is immediately retried and the acknowledged effect clears", async () => {
  const state = { pads: [], sounding: null }, calls = [];
  let failed = false;
  const send = async (pad, enabled) => { calls.push([pad, enabled]); if (!enabled && !failed) { failed = true; throw Error("rejected"); } };
  await pressFx(state, "A", same, send);
  await releaseFx(state, "A", same, send);
  assert.deepEqual(calls, [["A", true], ["A", false], ["A", false]]);
  assert.equal(state.sounding, null);
});

test("two failed OFF attempts report failure and a later release retries", async () => {
  const state = { pads: [], sounding: null }, calls = [];
  let attempts = 0;
  const send = async (pad, enabled) => { calls.push([pad, enabled]); if (!enabled && ++attempts <= 2) throw Error("rejected"); };
  await pressFx(state, "A", same, send);
  await assert.rejects(releaseFx(state, "A", same, send), /PAD FX の解除に失敗/);
  assert.equal(state.sounding, "A");
  await releaseFx(state, "A", same, send);
  assert.deepEqual(calls, [["A", true], ["A", false], ["A", false], ["A", false]]);
  assert.equal(state.sounding, null);
});

test("UI queue reports only an unrecovered failure and still runs later cleanup", async () => {
  const state = { pads: [], sounding: null }, queue = { tail: Promise.resolve() }, errors = [];
  let attempts = 0;
  const send = async (_pad, enabled) => { if (!enabled && ++attempts <= 2) throw Error("rejected"); };
  const report = error => errors.push(error.message);
  enqueuePadFx(queue, () => pressFx(state, "A", same, send), report);
  enqueuePadFx(queue, () => releaseFx(state, "A", same, send), report);
  enqueuePadFx(queue, () => releaseFx(state, "A", same, send), report);
  await queue.tail;
  assert.equal(errors.length, 1);
  assert.match(errors[0], /PAD FX の解除に失敗/);
  assert.equal(state.sounding, null);
});

test("two failed OFF attempts retain state for a later release-all cleanup", async () => {
  const state = { pads: [], sounding: null };
  let attempts = 0;
  const send = async (_pad, enabled) => { if (!enabled && ++attempts <= 2) throw Error("rejected"); };
  await pressFx(state, "A", same, send);
  await assert.rejects(releaseFx(state, "A", same, send));
  await releaseAllFx(state, send);
  assert.equal(attempts, 3);
  assert.equal(state.sounding, null);
});

function midiFixture(handler) {
  selectPads("A", 1);
  const calls = [], errors = [], decks = Object.fromEntries(["A", "B", "C", "D"].map(id => [id, { track: { trackId: id }, status: "paused" }]));
  const client = { getState: () => ({ snapshot: { decks, engine: { capabilities: [] }, mixer: { channels: {} } } }), getSessionId: () => "session", getDeckGeneration: () => 1,
    refreshSnapshot: async () => {}, setFx: async (deck, effect, enabled, mix, depth, beats) => { calls.push([effect, enabled]); await handler?.(effect, enabled); } };
  const actions = { activate() {}, library() {}, hotcue: async () => {}, cuePoints: { A: 0, B: 0, C: 0, D: 0 }, error: error => errors.push(error) };
  const runtime = new Ddj1000Runtime(client, () => actions);
  const event = (slot, pressed) => runtime.dispatch({ deck: "A", control: "padFx", mode: 1, slot, value: pressed ? 1 : 0, pressed });
  const wait = async () => { await runtime.queues.get("A"); };
  return { runtime, calls, errors, event, wait };
}

test("MIDI failed replacement and releases do not latch the previous effect", async () => {
  const f = midiFixture(async (effect, enabled) => { if (effect === "reverb" && enabled) throw Error("rejected"); });
  try {
    f.event(0, true); f.event(2, true); f.event(0, false); f.event(2, false); await f.wait();
    assert.deepEqual(f.calls, [["echo", true], ["reverb", true], ["echo", true], ["echo", false]]);
    assert.equal(f.errors.length, 1);
  } finally { f.runtime.dispose(); await f.wait(); }
});

test("MIDI reports two failed OFF attempts and retries on another release", async () => {
  let offAttempts = 0;
  const f = midiFixture(async (_effect, enabled) => { if (!enabled && ++offAttempts <= 2) throw Error("rejected"); });
  try {
    f.event(0, true); f.event(0, false); f.event(0, false); await f.wait();
    assert.deepEqual(f.calls, [["echo", true], ["echo", false], ["echo", false], ["echo", false]]);
    assert.equal(f.errors.length, 1);
    assert.match(f.errors[0], /PAD FX の解除に失敗/);
  } finally { f.runtime.dispose(); await f.wait(); }
});

test("MIDI reset waits for the pending press before releasing it", async () => {
  const gate = deferred();
  const f = midiFixture(async (effect, enabled) => { if (enabled) await gate.promise; });
  try {
    f.event(0, true); await new Promise(resolve => setImmediate(resolve));
    f.runtime.reset(); gate.resolve(); await f.wait();
    assert.deepEqual(f.calls, [["echo", true], ["echo", false]]);
  } finally { f.runtime.dispose(); await f.wait(); }
});

test("MIDI mode change releases the acknowledged effect after a failed switch", async () => {
  const f = midiFixture(async (effect, enabled) => { if (effect === "reverb" && enabled) throw Error("rejected"); });
  try {
    f.event(0, true); f.event(2, true); selectPads("A", 0); await f.wait();
    assert.deepEqual(f.calls, [["echo", true], ["reverb", true], ["echo", true], ["echo", false]]);
  } finally { f.runtime.dispose(); await f.wait(); }
});
