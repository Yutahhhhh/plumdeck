import assert from "node:assert/strict";
import test from "node:test";
import { registerHooks } from "node:module";
// This is a mocked engine. Junction must never be contacted.
registerHooks({ resolve(specifier, context, nextResolve) {
  if (specifier === "../junction/client.ts" && context.parentURL?.endsWith("/ddj1000-runtime.ts")) {
    return { url: "data:text/javascript,export%20async%20function%20junctionCommand()%20%7B%20throw%20new%20Error('unexpected%20Junction%20call')%20%7D", shortCircuit: true };
  }
  return nextResolve(specifier, context);
} });
const { Ddj1000Runtime } = await import("./ddj1000-runtime.ts");

const deferred = () => { let resolve; const promise = new Promise(r => { resolve = r; }); return { promise, resolve }; };
function fixture(enabled, gate) {
  let generation = 1;
  const calls = [], errors = [];
  const deck = { track: { trackId: "track", durationMs: 10000 }, status: "paused", loopRegion: { startMs: 1000, endMs: 5000, enabled } };
  const decks = { A: deck, B: { track: null }, C: { track: null }, D: { track: null } };
  const client = { getState: () => ({ snapshot: { decks, engine: { capabilities: [] }, mixer: { channels: {} } } }), getSessionId: () => "session", getDeckGeneration: () => generation,
    refreshSnapshot: async () => {},
    setLoop: async (id, start, end) => { calls.push(["set", start, end]); deck.loopRegion = { startMs: start, endMs: end, enabled: false }; if (gate) await gate.promise; },
    enableLoop: async (id, value) => { calls.push(["enable", value]); deck.loopRegion.enabled = value; } };
  const actions = { activate() {}, library() {}, hotcue: async () => {}, cuePoints: { A: 0, B: 0, C: 0, D: 0 }, error: error => errors.push(error) };
  const runtime = new Ddj1000Runtime(client, () => actions);
  return { runtime, deck, calls, errors, changeTrack: () => { generation++; }, run: control => runtime.dispatch({ deck: "A", control, value: 1, pressed: true }), wait: async () => { await runtime.queues.get("A"); } };
}

for (const [control, end] of [["loopHalf", 3000], ["loopDouble", 9000]]) {
  test(`${control} keeps an active loop enabled after resizing`, async () => {
    const f = fixture(true);
    try {
      f.run(control); await f.wait();
      assert.deepEqual(f.calls, [["set", 1000, end], ["enable", true]]);
      assert.equal(f.deck.loopRegion.enabled, true);
      assert.deepEqual(f.errors, []);
    } finally { f.runtime.dispose(); await f.wait(); }
  });
}

test("resizing a disabled stored loop keeps it disabled", async () => {
  const f = fixture(false);
  try {
    f.run("loopHalf"); await f.wait();
    assert.deepEqual(f.calls, [["set", 1000, 3000]]);
    assert.equal(f.deck.loopRegion.enabled, false);
  } finally { f.runtime.dispose(); await f.wait(); }
});

test("a changed deck generation prevents re-enabling on the new track", async () => {
  const gate = deferred(), f = fixture(true, gate);
  try {
    f.run("loopDouble"); await new Promise(resolve => setImmediate(resolve));
    f.changeTrack(); gate.resolve(); await f.wait();
    assert.deepEqual(f.calls, [["set", 1000, 9000]]);
  } finally { f.runtime.dispose(); await f.wait(); }
});
