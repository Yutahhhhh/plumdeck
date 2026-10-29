import test from "node:test";
import assert from "node:assert/strict";
import { defaultPadPreset, padOverrideOf, padPreset, PAD_FX_EXTENDED, setPadOverride, slotFromOverride } from "./pad-presets.ts";

test("an edited pad replaces the bank's slot for every deck and input", () => {
  setPadOverride(1, 2, { type: "beat:echo", beats: 1 / 4, param: 40 });
  const slot = padPreset(1, 2, [PAD_FX_EXTENDED]);
  assert.equal(slot.kind, "beat");
  assert.equal(slot.effect, "echo");
  assert.equal(slot.beats, 1 / 4);
  assert.equal(slot.mix, 0.4);
  setPadOverride(1, 2, null);
  assert.deepEqual(padPreset(1, 2, [PAD_FX_EXTENDED]), defaultPadPreset(1, 2, [PAD_FX_EXTENDED]));
});

test("an edit the host cannot play falls back to the bank's default", () => {
  setPadOverride(5, 0, { type: "beat:roll", beats: 1 / 8, param: 100 });
  assert.equal(padPreset(5, 0, [PAD_FX_EXTENDED]).effect, "roll");
  assert.deepEqual(padPreset(5, 0, []), defaultPadPreset(5, 0, []));
  setPadOverride(5, 0, null);
});

test("COLOR sweep below 50 is a low-pass and above 50 a high-pass; the editor round-trips", () => {
  const lpf = slotFromOverride({ type: "color:filter", beats: null, param: 10 });
  assert.equal(lpf.kind, "color"); assert.ok(lpf.amount < 0); assert.equal(lpf.variant, "LPF");
  assert.deepEqual(padOverrideOf(slotFromOverride({ type: "color:filter", beats: null, param: 80 })), { type: "color:filter", beats: null, param: 80 });
  assert.deepEqual(padOverrideOf(slotFromOverride({ type: "beat:reverb", beats: null, param: 50 })), { type: "beat:reverb", beats: null, param: 50 });
});

test("invalid edits are ignored", () => {
  setPadOverride(1, 3, { type: "beat:nonsense", beats: 3, param: 500 });
  assert.deepEqual(padPreset(1, 3, []), defaultPadPreset(1, 3, []));
});
