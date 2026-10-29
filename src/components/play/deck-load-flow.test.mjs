import test from "node:test";
import assert from "node:assert/strict";
import { DeckLoadError, runDeckLoad } from "./deck-load-flow.ts";

const never = () => new Promise(() => {});

function steps(overrides = {}) {
  const calls = [];
  let polls = 0;
  const base = {
    trackId: "7",
    current: () => true,
    ensureEngine: async () => { calls.push("engine"); },
    ensureAudio: async () => { calls.push("audio"); },
    fetchMetadata: async () => { calls.push("metadata"); return { beat_grid: { bpm: 120 } }; },
    prepare: (metadata) => { calls.push(["prepare", metadata]); },
    load: async () => { calls.push("load"); },
    poll: async () => { polls++; return polls < 3 ? { status: "loading", trackId: null } : { status: "ready", trackId: "7" }; },
    sleep: async () => {},
    engineTimeoutMs: 20, audioTimeoutMs: 20, metadataTimeoutMs: 20, loadTimeoutMs: 20, settleTimeoutMs: 1000,
  };
  return { calls, steps: { ...base, ...overrides } };
}

test("a load runs engine, audio, metadata, load and waits for the engine to report the track", async () => {
  const { calls, steps: s } = steps();
  const result = await runDeckLoad(s);
  assert.equal(result.kind, "loaded");
  assert.deepEqual(result.metadata, { beat_grid: { bpm: 120 } });
  assert.equal(result.metadataError, null);
  assert.deepEqual(calls.map((c) => (Array.isArray(c) ? c[0] : c)), ["engine", "audio", "metadata", "prepare", "load"]);
});

test("a stalled metadata GET does not block the load: it loads without a grid and reports why", async () => {
  const { calls, steps: s } = steps({ fetchMetadata: never });
  const result = await runDeckLoad(s);
  assert.equal(result.kind, "loaded");
  assert.equal(result.metadata, null);
  assert.match(result.metadataError, /グリッド/);
  assert.deepEqual(calls.find((c) => Array.isArray(c)), ["prepare", null]);
  assert.ok(calls.includes("load"));
});

test("a failing metadata GET still loads the track", async () => {
  const { steps: s } = steps({ fetchMetadata: async () => { throw new Error("HTTP 500"); } });
  const result = await runDeckLoad(s);
  assert.equal(result.kind, "loaded");
  assert.match(result.metadataError, /HTTP 500/);
});

test("a stalled engine start fails with a message that names the step instead of doing nothing", async () => {
  const { steps: s } = steps({ ensureEngine: never });
  await assert.rejects(runDeckLoad(s), (error) => error instanceof DeckLoadError && error.stage === "engine");
});

test("an engine that never answers the load request fails visibly", async () => {
  const { steps: s } = steps({ load: never });
  await assert.rejects(runDeckLoad(s), (error) => error instanceof DeckLoadError && error.stage === "load");
});

test("an engine error while loading is reported with the engine's message", async () => {
  const { steps: s } = steps({ poll: async () => ({ status: "error", lastError: "file unreadable" }) });
  await assert.rejects(runDeckLoad(s), /file unreadable/);
});

test("a load that never settles times out", async () => {
  let time = 0;
  const { steps: s } = steps({ poll: async () => ({ status: "loading" }), now: () => time, sleep: async (ms) => { time += ms; }, settleTimeoutMs: 200 });
  await assert.rejects(runDeckLoad(s), (error) => error instanceof DeckLoadError && error.stage === "settle");
});

test("a replaced load stops without loading", async () => {
  let wanted = true;
  const { calls, steps: s } = steps({ ensureAudio: async () => { wanted = false; }, current: () => wanted });
  const result = await runDeckLoad(s);
  assert.equal(result.kind, "superseded");
  assert.ok(!calls.includes("load"));
});
