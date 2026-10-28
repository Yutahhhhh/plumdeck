import test from "node:test";
import assert from "node:assert/strict";
import { awaitPendingGrid } from "./pending-grid.ts";

const clock = () => {
  let time = 0;
  return { now: () => time, sleep: async (ms) => { time += ms; } };
};

test("a grid analyzed after the load reaches the deck on a later GET", async () => {
  const answers = [{ beat_grid: null, grid_pending: true }, { beat_grid: { bpm: 120 }, grid_pending: false }];
  let calls = 0;
  const result = await awaitPendingGrid({ ...clock(), fetch: async () => answers[calls++], current: () => true });
  assert.deepEqual(result, answers[1]);
  assert.equal(calls, 2);
});

test("a replaced load stops waiting without another GET", async () => {
  let calls = 0;
  const result = await awaitPendingGrid({ ...clock(), fetch: async () => { calls++; return { beat_grid: null, grid_pending: true }; }, current: () => false });
  assert.equal(result, null);
  assert.equal(calls, 0);
});

test("a failed analysis ends the wait with its warning, a timeout ends it quietly", async () => {
  const failed = { beat_grid: null, grid_pending: false, grid_warning: "analysis: cannot decode" };
  const ended = await awaitPendingGrid({ ...clock(), fetch: async () => failed, current: () => true });
  assert.deepEqual(ended, failed);
  let calls = 0;
  const timedOut = await awaitPendingGrid({ ...clock(), intervalMs: 1000, timeoutMs: 5000, current: () => true,
    fetch: async () => { calls++; if (calls === 2) throw new Error("offline"); return { beat_grid: null, grid_pending: true }; } });
  assert.equal(timedOut, null);
  assert.equal(calls, 5);
});
