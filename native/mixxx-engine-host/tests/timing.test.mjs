import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const baseline = process.env.PLUMDECK_TIMING_BASELINE === '1';
test('audio callback timing: scratch release PCM and placement-only quantize including SYNC', { timeout: 60000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'plumdeck-timing-'));
  const rate = Number(process.env.PLUMDECK_TIMING_SOURCE_RATE || 44100), frames = rate * 40, wave = Buffer.alloc(44 + frames * 4);
  wave.write('RIFF'); wave.writeUInt32LE(wave.length - 8, 4); wave.write('WAVEfmt ', 8);
  wave.writeUInt32LE(16, 16); wave.writeUInt16LE(1, 20); wave.writeUInt16LE(2, 22);
  wave.writeUInt32LE(rate, 24); wave.writeUInt32LE(rate * 4, 28);
  wave.writeUInt16LE(4, 32); wave.writeUInt16LE(16, 34); wave.write('data', 36); wave.writeUInt32LE(frames * 4, 40);
  for (let n = 0; n < frames; n++) {
    // Non-integer frequency: a 500ms quantized seek must not be hidden by
    // landing at the same phase of a periodic 440Hz fixture.
    const v = Math.round(4000 * Math.sin(2 * Math.PI * 437.3 * n / rate));
    wave.writeInt16LE(v, 44 + n * 4); wave.writeInt16LE(v, 46 + n * 4);
  }
  const fixture = path.join(directory, 'tone.wav'); await writeFile(fixture, wave);
  const child = spawn(process.env.PLUMDECK_TEST_HOST || path.resolve(import.meta.dirname, '../build-upstream/plumdeck-mixxx-engine-host'), [], {
    env: { ...process.env, PLUMDECK_MIXXX_OUTPUT_DEVICE: process.env.PLUMDECK_MIXXX_OUTPUT_DEVICE || 'BlackHole 2ch', PLUMDECK_MIXXX_RECORDING_DIR: directory, PLUMDECK_MIXXX_TIMING_TRACE: '1' },
  });
  let id = 0, hello, stderr = '';
  const pending = new Map();
  child.stderr.on('data', data => { stderr = (stderr + data).slice(-12000); });
  createInterface({ input: child.stdout }).on('line', line => {
    const message = JSON.parse(line); if (message.kind !== 'event') pending.get(message.id)?.(message);
  });
  async function command(op, params = {}) {
    const current = ++id;
    const reply = new Promise((resolve, reject) => {
      const timer = setTimeout(() => { pending.delete(current); reject(new Error(`${op}: ${stderr}`)); }, 5000);
      pending.set(current, message => { clearTimeout(timer); pending.delete(current); resolve(message); });
    });
    child.stdin.write(JSON.stringify({ id: current, op, params, ...(hello ? { engineId: hello.engineId, sessionId: hello.sessionId } : {}) }) + '\n');
    const message = await reply; assert.notEqual(message.kind, 'error', JSON.stringify(message)); return message.data ?? message;
  }
  const snapshot = () => command('state.snapshot');
  async function until(predicate) {
    for (let n = 0; n < 200; n++) { const state = await snapshot(); if (predicate(state)) return state; await delay(20); }
    throw new Error(`State timeout: ${stderr}`);
  }
  const trace = async (deck = 'A') => {
    const result = await command('deck.timing.trace', { deck });
    assert.equal(result.dropped, 0); return result;
  };
  const evidence = { directory, baseline, sourceRate: rate };
  const trialTraces = [], quantizeTraces = [];
  const check = (condition, message) => { if (!baseline) assert(condition, message); };
  const jumps = rows => rows.slice(1).map((r, i) => r.positionMs - rows[i].positionMs - (r.commandedSpeed + rows[i].commandedSpeed) * (r.commandedSpeed * rows[i].commandedSpeed < 0 ? .25 : .5) * r.callbackMs);
  try {
    hello = await command('session.hello'); await until(s => s.audio.applied);
    for (const deck of ['A', 'B']) {
      await command('deck.load', { deck, track: { trackId: deck, path: fixture, bpm: 120, beatgridOffsetMs: 125 } });
      await until(s => s.decks[deck].track);
      await command('deck.seek', { deck, positionMs: deck === 'A' ? 5000 : 5230 });
    }
    await delay(300);
    await command('recording.start');
    const recording = (await until(s => s.recording.active)).recording;
    await delay(700); await trace();
    await command('deck.play', { deck: 'A' }); await delay(700);
    const onset = (await trace()).rows;
    const playingOnset = onset.find(r => r.speed > 0.1);
    assert(playingOnset, `Audio callback transport is running: ${JSON.stringify({ rows: onset.slice(-5), state: (await snapshot()).decks.A })}`);
    const steady = onset.slice(-50);
    evidence.steady = { minSpeed: Math.min(...steady.map(r => r.speed)), maxSpeed: Math.max(...steady.map(r => r.speed)),
      monotonic: steady.slice(1).every((r, i) => r.positionMs > steady[i].positionMs), maxJumpMs: Math.max(...jumps(steady).map(Math.abs)) };
    const scratch = (phase, positionMs) => command('deck.scratch', { deck: 'A', phase, positionMs, gestureId: 'timing' });
    await scratch('begin', 0); await delay(100);
    const started = performance.now();
    for (let step = 1; step <= 30; step++) {
      await scratch('move', -600 * step / 30);
      await delay(Math.max(0, started + step * 5 - performance.now()));
    }
    await scratch('end', -600);
    await delay(1200);
    const motion = (await trace()).rows;
    const firstHeld = motion.findIndex(r => r.held);
    const releaseIndex = motion.findIndex((r, i) => i > firstHeld && !r.held);
    assert(releaseIndex > 0, 'Trace contains the release edge');
    const release = motion[releaseIndex];
    const after = motion.slice(releaseIndex);
    const normal = after.find((r, i) => !r.scratching && after.slice(i).every(row => Math.abs(row.speed - 1) < .1));
    if (normal) assert(after.at(-1).audioMs - normal.audioMs >= 1000, 'Measure a full audio second after release');
    const target = release.targetMs;
    const releaseError = normal ? normal.positionMs - (target + normal.audioMs - release.audioMs + release.callbackMs) : 9999;
    evidence.scratch = {
      callbackMs: release.callbackMs, resumeDelayMs: normal ? normal.audioMs - release.audioMs : 9999,
      commandToNormalMs: normal ? release.requestAgeMs + normal.audioMs - release.audioMs : 9999,
      releaseErrorMs: releaseError, maxJumpMs: Math.max(...jumps(motion).map(Math.abs)),
      minPostReleaseSpeed: Math.min(...after.filter(r => normal && r.audioMs >= normal.audioMs).map(r => r.speed)), maxPostReleaseSpeed: Math.max(...after.filter(r => normal && r.audioMs >= normal.audioMs).map(r => r.speed)),
    };
    await command('recording.stop'); await until(s => !s.recording.active && !s.recording.stopping);
    await delay(700);
    const recorded = await readFile(recording.path);
    let pcm, pcmRate, channels;
    for (let offset = 12; offset + 8 <= recorded.length;) {
      const length = recorded.readUInt32LE(offset + 4), name = recorded.toString('ascii', offset, offset + 4);
      if (name === 'fmt ') { pcmRate = recorded.readUInt32LE(offset + 12); channels = recorded.readUInt16LE(offset + 10); }
      if (name === 'data') pcm = recorded.subarray(offset + 8, offset + 8 + length);
      offset += 8 + length + length % 2;
    }
    assert(pcm?.length > 0);
    const samples = Array.from({ length: pcm.length / (channels * 2) }, (_, i) => pcm.readInt16LE(i * channels * 2));
    const audible = samples.findIndex(v => Math.abs(v) > 30);
    assert(audible >= 0, 'Recorded PCM contains the transport onset');
    const releaseFrame = Math.round(audible + (release.audioMs - playingOnset.audioMs) * pcmRate / 1000);
    // Search a generous boundary window: record startup has a one-callback fade.
    const region = samples.slice(releaseFrame - Math.round(pcmRate * .05), releaseFrame + Math.round(pcmRate * .1));
    assert(region.length >= pcmRate * .149, 'Recorded PCM contains the entire release boundary window');
    let maxDelta = 0, silent = 0, maxSilent = 0;
    for (let i = 1; i < region.length; i++) {
      maxDelta = Math.max(maxDelta, Math.abs(region[i] - region[i - 1]));
      silent = Math.abs(region[i]) < 10 ? silent + 1 : 0; maxSilent = Math.max(maxSilent, silent);
    }
    evidence.pcm = { recording: recording.path, maxAdjacentDelta: maxDelta, maxSilentMs: maxSilent * 1000 / pcmRate };
    evidence.releaseTrials = [];
    for (const [offset, interval, keylock = false] of [[-600, 5], [600, 5], [-900, 5], [900, 16], [-600, 25], [600, 25], [-600, 5, true]]) {
      await command('deck.keylock.set', { deck: 'A', enabled: keylock });
      await command('deck.seek', { deck: 'A', positionMs: 5000 }); await delay(120); await trace();
      await scratch('begin', 0); await delay(25);
      const start = performance.now(), steps = Math.ceil(150 / interval);
      for (let step = 1; step <= steps; step++) {
        await scratch('move', offset * step / steps);
        await delay(Math.max(0, start + step * 150 / steps - performance.now()));
      }
      await scratch('end', offset); await delay(1100);
      const rows = (await trace()).rows;
      trialTraces.push({ offset, interval, keylock, rows });
      const held = rows.findIndex(r => r.held);
      const index = rows.findIndex((r, i) => i > held && !r.held);
      assert(index > 0);
      const release = rows[index], after = rows.slice(index);
      const normal = after.find((r, i) => !r.scratching && after.slice(i).every(row => Math.abs(row.speed - 1) < .1));
      assert(normal);
      assert(after.at(-1).audioMs - normal.audioMs >= 1000, 'Every trial includes a full audio second at stable rate');
      const stable = after.filter(r => r.audioMs >= normal.audioMs);
      const trial = { offset, interval, keylock, commandToNormalMs: release.requestAgeMs + normal.audioMs - release.audioMs,
        errorMs: normal.positionMs - (release.targetMs + normal.audioMs - release.audioMs + release.callbackMs),
        minSpeed: Math.min(...stable.map(r => r.speed)), maxSpeed: Math.max(...stable.map(r => r.speed)), maxJumpMs: Math.max(...jumps(rows.slice(index - 1)).map(Math.abs)) };
      evidence.releaseTrials.push(trial);
    }
    // SYNC ON aligns the follower's phase once (H-3, interim on the Mixxx
    // host) and never bends tempo. Observe BOTH decks so automatic leader
    // selection cannot hide which deck moved.
    for (const deck of ['A', 'B']) {
      await command('deck.pause', { deck });
      await command('deck.sync.set', { deck, enabled: false });
      await command('deck.seek', { deck, positionMs: deck === 'A' ? 10000 : 10230 });
      await command('deck.quantize.set', { deck, enabled: true });
    }
    await delay(150);
    for (const deck of ['A', 'B']) await command('deck.play', { deck });
    await delay(250); await trace('A'); await trace('B');
    await command('deck.sync.set', { deck: 'B', enabled: true });
    await command('deck.sync.set', { deck: 'A', enabled: true });
    await delay(400);
    evidence.syncStart = [];
    for (const deck of ['A', 'B']) {
      const result = await trace(deck);
      const moves = jumps(result.rows).map(Math.abs);
      // The row carrying the one-shot phase seek reports it as speed; judge
      // tempo on the rows around it.
      const steady = result.rows.filter((_, i) => !i || moves[i - 1] < 1);
      evidence.syncStart.push({ deck, maxJumpMs: Math.max(...moves), jumpCount: moves.filter(jump => jump >= 1).length, minSpeed: Math.min(...steady.map(r => r.speed)), maxSpeed: Math.max(...steady.map(r => r.speed)) });
    }
    evidence.quantize = [];
    await command('deck.play', { deck: 'B' });
    for (const sync of [false, true]) {
      await command('deck.sync.set', { deck: 'B', enabled: sync });
      await command('deck.sync.set', { deck: 'A', enabled: sync });
      await delay(400); await trace();
      for (const enabled of [true, false, true]) {
        await command('deck.quantize.set', { deck: 'A', enabled });
        await command('deck.hotcue.set', { deck: 'A', index: 3 });
        await command('deck.loop.set', { deck: 'A', startMs: 20000, endMs: 22000 });
        await command('deck.loop.beats', { deck: 'A', beats: 4 });
        await delay(300);
        const result = await trace(), rows = result.rows;
        quantizeTraces.push({ sync, enabled, ...result });
        evidence.quantize.push({ sync, enabled, nativeQuantize: result.nativeQuantize, maxJumpMs: Math.max(...jumps(rows).map(Math.abs)), minSpeed: Math.min(...rows.map(r => r.speed)), maxSpeed: Math.max(...rows.map(r => r.speed)) });
        await command('deck.loop.enable', { deck: 'A', enabled: false });
      }
    }
    await command('deck.pause', { deck: 'A' });
    await command('deck.seek', { deck: 'A', positionMs: 1360 }); await delay(150); await trace();
    for (const enabled of [true, false, true]) {
      await command('deck.quantize.set', { deck: 'A', enabled });
      await command('deck.hotcue.set', { deck: 'A', index: 4 });
      await command('deck.loop.beats', { deck: 'A', beats: 4 });
      await delay(100);
      await command('deck.loop.enable', { deck: 'A', enabled: false });
    }
    const paused = (await trace()).rows;
    evidence.pausedPlacementMaxErrorMs = Math.max(...paused.map(r => Math.abs(r.positionMs - 1360)));
    await writeFile(path.join(directory, 'evidence.json'), JSON.stringify({ ...evidence, onset, motion, trialTraces, quantizeTraces, paused }, null, 2));
    console.log(JSON.stringify(evidence));
    check(evidence.scratch.resumeDelayMs < 30, 'release resumes in <30ms');
    check(evidence.steady.monotonic && evidence.steady.minSpeed > .999 && evidence.steady.maxSpeed < 1.001 && evidence.steady.maxJumpMs < 1, 'steady playback is monotonic and constant-speed');
    check(evidence.scratch.commandToNormalMs < 30, 'release including command-to-callback wait resumes in <30ms');
    check(Math.abs(evidence.scratch.releaseErrorMs) < 20, 'release lands within 20ms');
    // The scaler carries fractional sample history across direction changes;
    // its integrated position can differ by ~1ms from the ideal linear ramp.
    check(evidence.scratch.maxJumpMs < 2, 'scratch follows the continuous rate-ramp model');
    check(evidence.scratch.minPostReleaseSpeed >= .9 && evidence.scratch.maxPostReleaseSpeed <= 1.1, 'stable transport for at least 1s after the bounded release transition');
    check(evidence.pcm.maxSilentMs < 2, 'no release silence gap');
    check(evidence.pcm.maxAdjacentDelta < 1600, 'no release PCM click');
    for (const trial of evidence.releaseTrials) {
      check(trial.commandToNormalMs < 30 && Math.abs(trial.errorMs) < 20, `release timing/position: ${JSON.stringify(trial)}`);
      check(trial.minSpeed >= .9 && trial.maxSpeed <= 1.1 && trial.maxJumpMs < 2, `release continuity: ${JSON.stringify(trial)}`);
    }
    for (const item of evidence.quantize) {
      check(item.nativeQuantize === 0, 'native quantize remains off');
      check(item.maxJumpMs < 1, 'placement must not seek');
      check(item.minSpeed > .999 && item.maxSpeed < 1.001, 'placement must not phase-bend even with SYNC');
    }
    const moved = evidence.syncStart.filter(item => item.maxJumpMs >= 1);
    check(moved.length <= 1 && moved.every(item => item.jumpCount === 1), 'SYNC at equal BPM moves only the follower, once');
    for (const item of evidence.syncStart) {
      check(item.minSpeed > .999 && item.maxSpeed < 1.001, 'SYNC at equal BPM must not phase-bend either deck');
    }
    check(evidence.pausedPlacementMaxErrorMs < .1, 'paused cue/loop placement and quantize toggling preserve position');
  } finally { child.stdin.end(); child.kill('SIGTERM'); }
});
