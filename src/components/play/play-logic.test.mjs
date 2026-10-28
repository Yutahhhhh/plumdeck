import test from "node:test";
import assert from "node:assert/strict";
import { beatLoopPress, formatBeats, loopBeats, manualLoopOut, resizeLoop, snapToBeat, stepBeats, LOOP_BEATS_RANGE, LOOP_BEATS_RANGE_EXTENDED } from "./loop-math.ts";
import { formatBpm, formatTempoPercent, stepTempo, tempoStep, TEMPO_RANGES } from "./tempo-math.ts";
import { faderGain, faderPosition, formatColorAmount, formatDb, FADER_CURVES, isKilled, nextDropouts, readCrossfaderCurve } from "./mixer-math.ts";
import { beatFxMaxBeats, beatFxMinBeats, beatFxHasBeats, BEAT_FX_MENU } from "../../services/midi/beat-fx.ts";
import { readFileSync, readdirSync } from "node:fs";
import { readShortcutPreset, resolveShortcut, shortcutHelp, shortcutTable, isMomentary } from "./keyboard-shortcuts.ts";
import { currentKey, formatKey, formatKeyDelta, formatLibraryKey, parseKey, shiftKey } from "../../services/dj-engine/musical-key.ts";
import { padFxParams, padPreset, INSERT_EFFECTS, PAD_FX_EXTENDED } from "../../services/midi/pad-presets.ts";
import { createPadFxSender, padFxGroup, padFxSounding } from "../../services/midi/pad-fx-send.ts";

/** Capabilities the Mixxx host declares today that PAD FX look at. */
const CURRENT_HOST = ["mixer.fx", "mixer.filter", "mixer.colorfx"];
import { pressPad, releasePad, emptyPadFxStack } from "./pad-fx-stack.ts";
import { chooseSyncLeader } from "./sync-leader.ts";

const grid120 = { bpm: 120, beatgridOffsetMs: 0 };

test("BPM keeps one rounding for whole and fraction", () => {
  assert.deepEqual(formatBpm(127.996), { whole: "128", fraction: "00" });
  assert.deepEqual(formatBpm(127.994), { whole: "127", fraction: "99" });
  assert.deepEqual(formatBpm(99.999), { whole: "100", fraction: "00" });
  assert.deepEqual(formatBpm(120), { whole: "120", fraction: "00" });
  for (let bpm = 60; bpm < 200; bpm += 0.0137) {
    const parts = formatBpm(bpm);
    assert.ok(Math.abs(Number(`${parts.whole}.${parts.fraction}`) - bpm) <= 0.005 + 1e-9, String(bpm));
    assert.equal(parts.fraction.length, 2);
  }
  assert.equal(formatBpm(null), null);
  assert.equal(formatBpm(0), null);
});

test("tempo ± follows the range resolution and stays inside the range", () => {
  assert.deepEqual(TEMPO_RANGES.map(tempoStep), [0.02, 0.04, 0.04, 0.5]);
  assert.ok(Math.abs(stepTempo(1, 1, 6) - 1.0002) < 1e-9);
  assert.ok(Math.abs(stepTempo(1, -1, 10) - 0.9996) < 1e-9);
  assert.ok(Math.abs(stepTempo(1, 1, 75) - 1.005) < 1e-9);
  assert.equal(stepTempo(1.06, 1, 6), 1.06);
  assert.equal(stepTempo(0.94, -3, 6), 0.94);
  assert.equal(formatTempoPercent(1.0002, 6), "+0.02%");
  assert.equal(formatTempoPercent(0.9996, 16), "−0.04%");
  assert.equal(formatTempoPercent(1, 6), "±0.00%");
  assert.equal(formatTempoPercent(1.105, 75), "+10.5%");
});

test("beat fractions are shown as fractions", () => {
  assert.equal(formatBeats(0.5), "1/2");
  assert.equal(formatBeats(0.75), "3/4");
  assert.equal(formatBeats(1 / 64), "1/64");
  assert.equal(formatBeats(0.125), "1/8");
  assert.equal(formatBeats(16), "16");
  assert.equal(formatBeats(0.3), "3/10");
  assert.equal(formatBeats(0.123456), "0.1235");
});

test("loop 1/2X and 2X keep the IN point", () => {
  const loop = { startMs: 4000, endMs: 6000 };
  assert.deepEqual(resizeLoop(loop, 0.5, 60_000, grid120, LOOP_BEATS_RANGE), { startMs: 4000, endMs: 5000 });
  assert.deepEqual(resizeLoop(loop, 2, 60_000, grid120, LOOP_BEATS_RANGE), { startMs: 4000, endMs: 8000 });
  assert.equal(resizeLoop(loop, 2, 7000, grid120, LOOP_BEATS_RANGE), null, "never past the end of the track");
  const tiny = { startMs: 0, endMs: 62.5 };
  assert.equal(resizeLoop(tiny, 0.5, 60_000, grid120, LOOP_BEATS_RANGE), null, "host range stops at 1/8 beat");
  assert.deepEqual(resizeLoop(tiny, 0.5, 60_000, grid120, LOOP_BEATS_RANGE_EXTENDED), { startMs: 0, endMs: 31.25 });
  assert.equal(stepBeats(64, 1, LOOP_BEATS_RANGE), 64);
  assert.equal(stepBeats(64, 1, LOOP_BEATS_RANGE_EXTENDED), 128);
  assert.equal(stepBeats(0.125, -1, LOOP_BEATS_RANGE), 0.125);
});

test("BEAT LOOP pad: same size exits, other size resizes from IN, idle creates", () => {
  const loop = { startMs: 4000, endMs: 6000 };
  assert.deepEqual(beatLoopPress(null, 4, grid120, 60_000, LOOP_BEATS_RANGE), { kind: "create", beats: 4 });
  assert.deepEqual(beatLoopPress(loop, 4, grid120, 60_000, LOOP_BEATS_RANGE), { kind: "exit" });
  assert.deepEqual(beatLoopPress(loop, 2, grid120, 60_000, LOOP_BEATS_RANGE), { kind: "resize", loop: { startMs: 4000, endMs: 5000 } });
  assert.equal(loopBeats(loop, grid120), 4);
});

test("quantize snaps to the nearest beat on fixed and exact grids", () => {
  assert.equal(snapToBeat(1260, grid120), 1500);
  assert.equal(snapToBeat(1240, grid120), 1000);
  assert.equal(snapToBeat(1260, { beatTimesMs: [0, 480, 1010, 1490, 2000] }), 1490);
  assert.equal(snapToBeat(777, {}), 777);
  assert.equal(snapToBeat(5, { bpm: 120, beatgridOffsetMs: 100 }), 100);
});

test("manual loop OUT needs a pending IN before the playhead", () => {
  assert.equal(manualLoopOut(null, 5000), null);
  assert.equal(manualLoopOut(5000, 4000), null);
  assert.deepEqual(manualLoopOut(2000, 4000), { startMs: 2000, endMs: 4000 });
});

test("key notation: Camelot by default, Classic on request", () => {
  for (const [text, camelot, classic] of [["A Minor", "8A", "Am"], ["C Major", "8B", "C"], ["F#m", "11A", "F#m"], ["Bbm", "3A", "Bbm"], ["Db Major", "3B", "Db"], ["E minor", "9A", "Em"], ["B", "1B", "B"], ["G# Minor", "1A", "G#m"]]) {
    const key = parseKey(text);
    assert.ok(key, text);
    assert.equal(formatKey(key, "camelot"), camelot, text);
    assert.equal(formatKey(key, "classic"), classic, text);
  }
  for (let number = 1; number <= 12; number++) for (const letter of ["A", "B"]) assert.equal(formatKey(parseKey(`${number}${letter}`), "camelot"), `${number}${letter}`);
  assert.equal(parseKey("unknown"), null);
  assert.equal(parseKey(""), null);
  assert.equal(formatLibraryKey("F Minor", "camelot"), "4A");
  assert.equal(formatLibraryKey("odd", "camelot"), "odd");
});

test("current key = library key + key shift, tempo added only without MT", () => {
  assert.equal(formatKey(shiftKey(parseKey("Cm"), -3), "camelot"), "8A", "KEY SYNC example: 5A moved −3 matches 8A");
  assert.deepEqual(currentKey("A Minor", 2, 1, true, "camelot"), { text: "10A", delta: 2, approximate: false });
  assert.deepEqual(currentKey("A Minor", 0, 1.06, true, "classic"), { text: "Am", delta: 0, approximate: false });
  const drift = currentKey("A Minor", 0, 1.06, false, "classic");
  assert.equal(drift.approximate, true);
  assert.equal(drift.delta, 1);
  assert.equal(drift.text, "Bbm");
  assert.equal(currentKey(null, 1, 1, true, "camelot").text, null);
  assert.equal(formatKeyDelta(0), "±0");
  assert.equal(formatKeyDelta(-3), "−3");
  assert.equal(formatKeyDelta(2), "+2");
});

test("mixer dB readouts and fader curves", () => {
  assert.equal(formatDb(0.5), "−6.0 dB");
  assert.equal(formatDb(1), "0.0 dB");
  assert.equal(formatDb(4), "+12.0 dB");
  assert.equal(formatDb(0), "−∞");
  assert.equal(isKilled(0), true);
  assert.equal(isKilled(1), false);
  for (const curve of FADER_CURVES) {
    assert.equal(faderGain(0, curve), 0);
    assert.equal(faderGain(1, curve), 1);
    for (let p = 0; p <= 1; p += 0.05) assert.ok(Math.abs(faderPosition(faderGain(p, curve), curve) - p) < 1e-9, `${curve} ${p}`);
  }
  assert.ok(faderGain(0.5, "slow") < faderGain(0.5, "linear"));
  assert.ok(faderGain(0.5, "fast") > faderGain(0.5, "linear"));
  assert.equal(formatColorAmount("filter", -0.5), "LPF 50%");
  assert.equal(formatColorAmount("echo", 0), "OFF");
});

test("Performance 1 table maps every key to its op", () => {
  const expected = {
    KeyZ: ["play", 0], KeyN: ["play", 1], KeyA: ["cue", 0], KeyH: ["cue", 1], KeyQ: ["sync", 0], KeyY: ["sync", 1],
    KeyD: ["loopIn", 0], KeyF: ["loopOut", 0], KeyG: ["reloop", 0], KeyK: ["loopIn", 1], KeyL: ["loopOut", 1], Semicolon: ["reloop", 1],
    KeyR: ["memoryPrevious", 0], KeyT: ["memoryNext", 0], KeyO: ["memoryPrevious", 1], KeyP: ["memoryNext", 1], KeyS: ["slipReverse", 0], KeyJ: ["slipReverse", 1],
  };
  for (const [code, [kind, side]] of Object.entries(expected)) assert.deepEqual(resolveShortcut("performance1", { code }), { kind, side }, code);
  assert.deepEqual(resolveShortcut("performance1", { code: "KeyQ", shiftKey: true }), { kind: "master", side: 0 });
  assert.deepEqual(resolveShortcut("performance1", { code: "KeyY", shiftKey: true }), { kind: "master", side: 1 });
  assert.deepEqual(resolveShortcut("performance1", { code: "KeyW" }), { kind: "bend", side: 0, direction: -1 });
  assert.deepEqual(resolveShortcut("performance1", { code: "KeyI" }), { kind: "bend", side: 1, direction: 1 });
  ["Digit1", "Digit2", "Digit3", "Digit4"].forEach((code, slot) => assert.deepEqual(resolveShortcut("performance1", { code }), { kind: "hotcue", side: 0, slot }));
  ["Digit6", "Digit7", "Digit8", "Digit9"].forEach((code, slot) => assert.deepEqual(resolveShortcut("performance1", { code }), { kind: "hotcue", side: 1, slot }));
  assert.deepEqual(resolveShortcut("performance1", { code: "Space" }), { kind: "layout" });
  assert.deepEqual(resolveShortcut("performance1", { code: "ArrowLeft", shiftKey: true }), { kind: "load", side: 0 });
  assert.equal(resolveShortcut("performance1", { code: "KeyZ", metaKey: true }), null, "system shortcuts stay with the OS");
  assert.equal(resolveShortcut("performance1", { code: "Digit5" }), null);
  assert.ok(isMomentary({ kind: "bend", side: 0, direction: 1 }));
  const codes = shortcutTable("performance1").map((entry) => `${entry.shift ? "S+" : ""}${entry.code}`);
  assert.equal(new Set(codes).size, codes.length, "no key is bound twice");
});

test("Performance 1 is the default preset; help pairs Deck 1 and Deck 2 keys", () => {
  assert.equal(readShortcutPreset(null), "performance1");
  assert.equal(readShortcutPreset("bogus"), "performance1");
  assert.equal(readShortcutPreset("classic"), "classic");
  const help = shortcutHelp("performance1");
  assert.deepEqual(help.find((row) => row.label === "BEAT SYNC")?.keys, ["Q", "Y"]);
  assert.deepEqual(help.find((row) => row.label === "HOT CUE A")?.keys, ["1", "6"]);
  assert.deepEqual(help.find((row) => row.label === "RELOOP / EXIT")?.keys, ["G", ";"]);
  assert.equal(help.find((row) => row.label === "レイアウト切替（ブラウザを広く）")?.keys, "Space");
  assert.ok(shortcutHelp("classic").every((row) => !Array.isArray(row.keys)));
});

test("classic preset keeps the previous keys", () => {
  assert.deepEqual(resolveShortcut("classic", { code: "Digit2" }), { kind: "selectDeck", deck: 1 });
  assert.deepEqual(resolveShortcut("classic", { code: "Space" }), { kind: "togglePlayActive" });
  assert.deepEqual(resolveShortcut("classic", { code: "KeyQ" }), { kind: "seekActive", deltaMs: -5000 });
});

test("PAD FX presets: no sine tremolo, inserts at full wet, one source for mouse and MIDI", () => {
  for (const caps of [[], CURRENT_HOST, [...CURRENT_HOST, PAD_FX_EXTENDED]]) for (const mode of [1, 5]) for (let slot = 0; slot < 16; slot++) {
    const preset = padPreset(mode, slot, caps);
    for (const name of preset.requires ?? []) assert.ok(caps.includes(name), `${mode}:${slot} needs ${name}`);
    if (preset.kind !== "beat") continue;
    // The generic tremolo is a near-silent sine; only the extended host turns it into the TRANS gate.
    if (!caps.includes(PAD_FX_EXTENDED)) assert.notEqual(preset.effect, "tremolo");
    if (INSERT_EFFECTS.includes(preset.effect)) assert.equal(padFxParams(preset, false).mix, 1);
    assert.equal(padFxParams(preset, false).beats, undefined, "no beats without the capability");
    assert.equal(padFxParams(preset, false).params, undefined, "no params without the capability");
    if (preset.beats) assert.equal(padFxParams(preset, true).beats, preset.beats);
  }
});

test("PAD FX banks follow the owner's assignment, then the redesign", () => {
  const extended = [...CURRENT_HOST, PAD_FX_EXTENDED];
  const names = (mode, caps) => Array.from({ length: 16 }, (_, slot) => {
    const preset = padPreset(mode, slot, caps);
    return `${preset.label}${preset.beats ? ` ${formatBeats(preset.beats)}` : preset.variant ? ` ${preset.variant}` : ""}`;
  });
  // Pads 1-8: the owner's PAD FX 1 / PAD FX 2 (Rust-only slots play the nearest host effect).
  assert.deepEqual(names(1, extended).slice(0, 8), ["ROLL 1/2", "FILTER LPF", "FLANGER 16", "ECHO 3/4", "ECHO 1/4", "ECHO 1/2", "REVERB 50%", "ECHO 1/2"]);
  assert.deepEqual(names(5, extended).slice(0, 8), ["SLIP ROLL 1/16", "SLIP ROLL 1/8", "SLIP ROLL 1/4", "SLIP ROLL 1/2", "ROLL 1/16", "ROLL 1/8", "ROLL 1/4", "ROLL 1/2"]);
  // Pads 9-16: the redesign's proposal.
  assert.deepEqual(names(1, extended).slice(8, 15), ["ECHO 1", "SPIRAL 1/2", "SLIP ROLL 1/8", "SLIP ROLL 1/16", "REVERB 60%", "TRANS 1/4", "FILTER LPF"]);
  assert.deepEqual(names(5, extended).slice(8, 15), ["REVERB LONG", "DUB ECHO", "PHASER 4", "PAN 1", "NOISE", "DELAY 1", "PITCH +"]);
  assert.deepEqual(padFxParams(padPreset(5, 13, extended), true).params, { feedback_amount: 0 });
  // Today's host: color pads work (Color FX), processor pads keep a working fallback.
  assert.equal(padPreset(1, 1, CURRENT_HOST).kind, "color");
  assert.equal(padPreset(1, 0, CURRENT_HOST).effect, "echo");
  assert.equal(padPreset(5, 0, CURRENT_HOST).effect, "echo");
  // Without Color FX either, the color pads fall back too.
  assert.equal(padPreset(1, 1, []).kind, "beat");
});

test("color pads borrow the COLOR FX and give the knob back after the last release", async () => {
  const calls = [];
  const channel = { deck: "A", gain: 1, eqLow: 1, eqMid: 1, eqHigh: 1, pfl: false, filter: 0.2, colorFx: { effect: "echo", amount: 0.2 } };
  const snapshot = { engine: { capabilities: CURRENT_HOST }, mixer: { channels: { A: channel } } };
  const client = {
    getState: () => ({ snapshot }),
    setFx: async (...args) => { calls.push(["fx", ...args.slice(0, 3)]); },
    setColorFx: async (deck, effect, amount) => { calls.push(["color", deck, effect, amount]); channel.colorFx = { effect, amount }; },
    setFilter: async (deck, value) => { calls.push(["filter", deck, value]); },
  };
  const sender = createPadFxSender(client);
  const sweep = padPreset(1, 1, CURRENT_HOST), noise = padPreset(5, 12, CURRENT_HOST);
  assert.equal(padFxGroup(sweep), "color");
  await sender.send("A", sweep, true);
  await sender.send("A", noise, true);
  assert.ok(padFxSounding(noise, channel));
  await sender.send("A", sweep, false);
  assert.deepEqual(calls, [["color", "A", "filter", -0.8], ["color", "A", "whitenoise", 0.5], ["color", "A", "echo", 0.2]]);
  const echo = padPreset(1, 0, CURRENT_HOST);
  assert.equal(padFxGroup(echo), "chain");
  await sender.send("A", echo, true);
  assert.deepEqual(calls.at(-1), ["fx", "A", "echo", true]);
  // A host with FILTER only drives the filter knob.
  const filterOnly = createPadFxSender({ ...client, getState: () => ({ snapshot: { ...snapshot, engine: { capabilities: ["mixer.filter"] } } }) });
  await filterOnly.send("A", padPreset(1, 14, ["mixer.filter"]), true);
  assert.deepEqual(calls.at(-1), ["filter", "A", -0.6]);
});

test("PAD FX presses stack: last pressed wins, release falls back to a held pad", () => {
  let stack = emptyPadFxStack;
  let step = pressPad(stack, 1); stack = step.stack;
  assert.deepEqual(step.commands, [{ slot: 1, enabled: true }]);
  step = pressPad(stack, 4); stack = step.stack;
  assert.deepEqual(step.commands, [{ slot: 4, enabled: true }]);
  step = pressPad(stack, 4); stack = step.stack;
  assert.deepEqual(step.commands, [], "repeat press is ignored");
  step = releasePad(stack, 4); stack = step.stack;
  assert.deepEqual(step.commands, [{ slot: 1, enabled: true }], "the still-held pad sounds again");
  step = releasePad(stack, 1); stack = step.stack;
  assert.deepEqual(step.commands, [{ slot: 1, enabled: false }]);
  assert.deepEqual(stack.held, []);
  step = pressPad(stack, 2); stack = step.stack; step = pressPad(stack, 3); stack = step.stack;
  step = releasePad(stack, 2);
  assert.deepEqual(step.commands, [], "releasing a pad that is not sounding changes nothing");
});

test("sync leader: MASTER first, then a playing deck, never an empty deck", () => {
  const decks = (overrides) => ({ A: { track: {}, status: "paused", syncLeader: null, syncEnabled: false }, B: { track: null, status: "empty", syncLeader: null, syncEnabled: false }, C: { track: {}, status: "playing", syncLeader: null, syncEnabled: false }, D: { track: null, status: "empty", syncLeader: null, syncEnabled: false }, ...overrides });
  assert.equal(chooseSyncLeader("A", ["A", "B", "C", "D"], decks({})), "C");
  assert.equal(chooseSyncLeader("A", ["A", "B", "C", "D"], decks({ D: { track: {}, status: "paused", syncLeader: "D", syncEnabled: true } })), "D");
  assert.equal(chooseSyncLeader("C", ["A", "B", "C", "D"], decks({})), "A");
  assert.equal(chooseSyncLeader("A", ["A", "B"], decks({})), undefined);
});

test("crossfader curve and dropout counting", () => {
  assert.equal(readCrossfaderCurve(null), "sharp");
  assert.equal(readCrossfaderCurve("sharp"), "sharp");
  assert.equal(readCrossfaderCurve("steep"), "sharp");
  let state = nextDropouts(null, "s1", 7, 1000);
  assert.deepEqual(state, { session: "s1", baseline: 7, count: 0, lastAt: null }, "counts start at the first reading");
  state = nextDropouts(state, "s1", 7, 2000);
  assert.equal(state.lastAt, null);
  state = nextDropouts(state, "s1", 9, 3000);
  assert.deepEqual([state.count, state.lastAt], [2, 3000]);
  state = nextDropouts(state, "s1", 9, 4000);
  assert.deepEqual([state.count, state.lastAt], [2, 3000], "no new dropout keeps the last time");
  assert.equal(nextDropouts(state, "s2", 9, 5000).count, 0, "a new session starts over");
  assert.equal(nextDropouts(state, "s1", 1, 5000).count, 0, "a restarted host starts over");
});

test("BEAT FX beat range follows the host and hides MOBIUS", () => {
  assert.equal(beatFxMaxBeats("echo"), 2);
  assert.equal(beatFxMinBeats("echo"), 1 / 8);
  assert.equal(beatFxMaxBeats("echo", ["mixer.beatfx.range.v2"]), 16);
  assert.equal(beatFxMinBeats("roll", ["mixer.beatfx.range.v2"]), 1 / 16);
  assert.equal(beatFxHasBeats("reverb"), false);
  assert.equal(beatFxHasBeats("pitchshift"), false);
  assert.ok(BEAT_FX_MENU.every(([effect]) => !effect.startsWith("mobius")));
});

test("Play styles keep text at 10px or larger", () => {
  const dir = new URL(".", import.meta.url);
  const small = [];
  for (const file of readdirSync(dir).filter((name) => name.endsWith(".css"))) {
    readFileSync(new URL(file, dir), "utf8").split("\n").forEach((line, index) => {
      for (const match of line.matchAll(/font-size:\s*([\d.]+)px/g)) if (Number(match[1]) < 10) small.push(`${file}:${index + 1} ${match[0]}`);
      for (const match of line.matchAll(/font:[^;]*?\b([\d.]+)px/g)) if (Number(match[1]) < 10) small.push(`${file}:${index + 1} ${match[0]}`);
    });
  }
  assert.deepEqual(small, []);
});
