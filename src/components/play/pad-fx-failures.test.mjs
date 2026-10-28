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
  const f = midiFixture(async (effect, enabled) => { if (effect === "flanger" && enabled) throw Error("rejected"); });
  try {
    f.event(0, true); f.event(2, true); f.event(0, false); f.event(2, false); await f.wait();
    assert.deepEqual(f.calls, [["echo", true], ["flanger", true], ["echo", true], ["echo", false]]);
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
  const f = midiFixture(async (effect, enabled) => { if (effect === "flanger" && enabled) throw Error("rejected"); });
  try {
    f.event(0, true); f.event(2, true); selectPads("A", 0); await f.wait();
    assert.deepEqual(f.calls, [["echo", true], ["flanger", true], ["echo", true], ["echo", false]]);
  } finally { f.runtime.dispose(); await f.wait(); }
});

const { createPadFxSender, padFxSounding, padFxCaption } = await import("../../services/midi/pad-fx-send.ts");
const { padPreset } = await import("../../services/midi/pad-presets.ts");

function colorClient(knob = { effect: "filter", amount: 0.3 }) {
  const calls = [];
  const channel = { colorFx: { ...knob }, filter: 0 };
  const client = {
    getState: () => ({ snapshot: { engine: { capabilities: ["mixer.colorfx", "mixer.filter"] }, mixer: { channels: { A: channel } } } }),
    setFx: async () => {},
    setColorFx: async (deck, effect, amount) => { calls.push([deck, effect, amount]); channel.colorFx = { effect, amount }; },
    setFilter: async () => {},
  };
  return { client, calls };
}
const LPF = { kind: "color", effect: "filter", label: "FILTER", amount: -0.8, hold: false };
const NOISE = { kind: "color", effect: "whitenoise", label: "NOISE", amount: 0.5, hold: false };

test("a COLOR pad held across a load still gives the knob back", async () => {
  const { client, calls } = colorClient();
  const sender = createPadFxSender(client);
  await sender.send("A", LPF, true);
  // Load: the release can no longer reach the old track, but COLOR FX is channel state.
  await sender.abandon("A");
  assert.deepEqual(calls, [["A", "filter", -0.8], ["A", "filter", 0.3]]);
  // Nothing left to give back.
  await sender.abandon("A");
  assert.equal(calls.length, 2);
});

test("overlapping COLOR pads from two sources restore the knob in either release order", async () => {
  for (const firstRelease of ["mouse", "midi"]) {
    const { client, calls } = colorClient();
    const sources = { mouse: createPadFxSender(client), midi: createPadFxSender(client) };
    await sources.mouse.send("A", LPF, true);
    await sources.midi.send("A", NOISE, true);
    const other = firstRelease === "mouse" ? "midi" : "mouse";
    await sources[firstRelease].send("A", firstRelease === "mouse" ? LPF : NOISE, false);
    await sources[other].send("A", other === "mouse" ? LPF : NOISE, false);
    const remaining = firstRelease === "mouse" ? ["A", "whitenoise", 0.5] : ["A", "filter", -0.8];
    assert.deepEqual(calls, [["A", "filter", -0.8], ["A", "whitenoise", 0.5], remaining, ["A", "filter", 0.3]], firstRelease);
  }
});

test("MIDI COLOR pad released after a load gives the knob back", async () => {
  selectPads("A", 1);
  const { client: color, calls } = colorClient();
  let generation = 1;
  const decks = Object.fromEntries(["A", "B", "C", "D"].map(id => [id, { track: { trackId: id }, status: "paused" }]));
  const state = color.getState().snapshot;
  const client = { ...color, getState: () => ({ snapshot: { ...state, decks } }), getSessionId: () => "session", getDeckGeneration: () => generation, refreshSnapshot: async () => {} };
  const actions = { activate() {}, library() {}, hotcue: async () => {}, cuePoints: { A: 0, B: 0, C: 0, D: 0 }, error: () => {} };
  const runtime = new Ddj1000Runtime(client, () => actions);
  try {
    // Bank 1 pad 2 is the COLOR low-pass on hosts with a filter.
    assert.equal(padPreset(1, 1, ["mixer.filter"]).kind, "color");
    runtime.dispatch({ deck: "A", control: "padFx", mode: 1, slot: 1, value: 1, pressed: true }); await runtime.queues.get("A");
    generation = 2;
    runtime.dispatch({ deck: "A", control: "padFx", mode: 1, slot: 1, value: 0, pressed: false }); await runtime.queues.get("A");
    assert.deepEqual(calls, [["A", "filter", -0.8], ["A", "filter", 0.3]]);
  } finally { runtime.dispose(); await runtime.queues.get("A"); }
});

test("PAD FX lamps and caption name only the pad that sounds", () => {
  const capabilities = ["mixer.fx.beats"];
  const echo = beats => ({ fx: { effect: "echo", enabled: true, mix: 0.5, depth: 0.55, beats } });
  // Bank 1: pad 5 is ECHO 1/4, pad 6 ECHO 1/2.
  assert.equal(padFxSounding(padPreset(1, 4, capabilities), echo(1 / 4), capabilities), true);
  assert.equal(padFxSounding(padPreset(1, 5, capabilities), echo(1 / 4), capabilities), false);
  assert.equal(padFxCaption(1, echo(1 / 4), capabilities), "ECHO 1/4");
  assert.equal(padFxCaption(1, { fx: { effect: "sliproll", enabled: true, mix: 1, depth: 1, beats: 3 } }, capabilities), "SLIP ROLL");
  assert.equal(padFxCaption(1, { colorFx: { effect: "filter", amount: -0.8 } }, ["mixer.filter"]), "FILTER LPF");
  assert.equal(padFxCaption(1, { fx: { effect: "echo", enabled: false, mix: 0.5 } }, capabilities), null);
});
