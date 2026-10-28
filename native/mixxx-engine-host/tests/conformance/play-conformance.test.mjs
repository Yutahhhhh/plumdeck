// Play-mode engine conformance (redesign spec 2.1 / 2.7.2 QB-1..QB-9 and the
// H-x host items). Opt-in: set PLUMDECK_TEST_HOST to a built host binary. The
// host must apply "BlackHole 2ch" or the suite refuses to load any audio. It
// never touches MIDI or the system output. Each check records its numbers in
// a report (PLUMDECK_CONFORMANCE_REPORT=<file.json>) so a host before and after
// a change, or the Mixxx host and a future Rust engine, can be compared.
import test from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { CommandError, FIXTURES, Host, RATE, clickTemplate, delay, workspace, writeFixture } from './harness.mjs';
import { db, dominantFrequency, findCopies, median, periodByAutocorrelation, residualDb, risingEdges, toneAmplitude, worstOutOfBandDb, peak, envelope } from './analysis.mjs';

const binary = process.env.PLUMDECK_TEST_HOST;
const reportPath = process.env.PLUMDECK_CONFORMANCE_REPORT;
// Optional comma-separated check ids (e.g. "H-3,QB-1") to run a subset.
const DECKS = ['A', 'B', 'C', 'D'];
const BEAT_MS = 500; // Every fixture grid is 120 BPM unless noted.
const round = value => typeof value === 'number' ? Math.round(value * 1000) / 1000 : value;
const ms = seconds => seconds * 1000;

/** Second click template (different noise), so two decks can be told apart. */
function otherClick() {
  let state = 11;
  const next = () => { state = (state * 1664525 + 1013904223) >>> 0; return state / 2 ** 32 * 2 - 1; };
  const length = Math.round(RATE * 0.001), out = new Float64Array(length);
  for (let i = 0; i < length; ++i) out[i] = 0.5 * next() * Math.sin(Math.PI * i / (length - 1));
  return out;
}
const CLICK_A = clickTemplate(), CLICK_B = otherClick();
FIXTURES.beatA = { seconds: 40, bpm: 120, render: n => (n % (RATE / 2) < CLICK_A.length ? CLICK_A[n % (RATE / 2)] : 0) };
FIXTURES.beatB = { seconds: 40, bpm: 120, render: n => (n % (RATE / 2) < CLICK_B.length ? CLICK_B[n % (RATE / 2)] : 0) };
// 128 BPM: 20671.875 frames per beat, so place each click at its rounded beat frame.
FIXTURES.beatB128 = { seconds: 40, bpm: 128, render: n => { const k = n - Math.round(Math.round(n / (RATE * 60 / 128)) * RATE * 60 / 128); return k >= 0 && k < CLICK_B.length ? CLICK_B[k] : 0; } };

const BEAT_FX = ['echo', 'reverb', 'lowcutecho', 'mtdelay', 'spiral', 'enigmajet', 'sliproll', 'roll', 'tremolo', 'flanger', 'phaser', 'pitchshift'];
const PAD_BASE = ['echo', 'reverb', 'flanger', 'phaser', 'filter', 'bitcrusher', 'distortion', 'autopan', 'tremolo', 'moogladder4filter'];
const PAD_PROCESSORS = ['lowcutecho', 'mtdelay', 'spiral', 'enigmajet', 'sliproll', 'roll', 'pitchshift'];

test('Play engine conformance on BlackHole 2ch', { timeout: 900000, skip: !binary && 'Set PLUMDECK_TEST_HOST to a built native host' }, async t => {
  const dir = await workspace();
  const files = {};
  for (const name of Object.keys(FIXTURES)) { files[name] = path.join(dir, `${name}.wav`); await writeFixture(files[name], name); }
  const h = await Host.start(binary, dir);
  const report = { host: binary, label: process.env.PLUMDECK_CONFORMANCE_LABEL ?? null, startedAt: new Date().toISOString(),
    output: h.audio.deviceId, sampleRateHz: h.audio.sampleRateHz, bufferFrames: h.audio.bufferFrames,
    capabilities: h.engine.capabilities, checks: [] };
  const padEffect = {};
  t.after(async () => {
    try { report.rssMb = Number(execFileSync('ps', ['-o', 'rss=', '-p', String(h.child.pid)], { encoding: 'utf8' }).trim()) / 1024; } catch { /* exited */ }
    await h.close();
    await rm(dir, { recursive: true, force: true });
    report.finishedAt = new Date().toISOString();
    if (reportPath) await writeFile(reportPath, JSON.stringify(report, null, 2));
    for (const check of report.checks) {
      console.log(`${check.status.toUpperCase().padEnd(11)} ${check.id.padEnd(14)} ${check.title}`);
      for (const c of check.criteria) console.log(`   ${c.pass ? 'ok  ' : 'FAIL'} ${c.name}${c.detail === undefined ? '' : ` -> ${JSON.stringify(c.detail)}`}`);
      if (check.error) console.log(`   error: ${check.error}`);
    }
  });

  // ---------------------------------------------------------------- helpers
  async function pad(deck, effect, enabled, extra = {}) {
    if (enabled) padEffect[deck] = effect;
    return h.try('mixer.fx.set', { deck, effect, enabled, mix: extra.mix ?? 0.5, depth: extra.depth ?? 0.5, ...extra });
  }
  async function neutral() {
    for (const deck of DECKS) {
      await h.try('deck.pause', { deck });
      await h.try('mixer.channel.gain', { deck, gain: 0 });
      await h.try('mixer.channel.orientation', { deck, orientation: 1 });
      await h.try('deck.sync.set', { deck, enabled: false });
      await h.try('deck.slip.set', { deck, enabled: false });
      await h.try('deck.reverse.set', { deck, enabled: false });
      await h.try('deck.loop.enable', { deck, enabled: false });
      // Keylock runs audio through the time stretcher, which smears clicks.
      await h.try('deck.key.reset', { deck });
      await h.try('deck.keylock.set', { deck, enabled: false });
      await h.try('deck.tempo.set', { deck, rate: 1 });
      if (padEffect[deck]) { await pad(deck, padEffect[deck], false); delete padEffect[deck]; }
    }
    await h.try('mixer.beatfx.set', { enabled: false, target: 'A', auto: true });
    await h.try('mixer.crossfader', { position: 0 });
    await h.try('mixer.master.gain', { gain: 0.5 });
  }
  async function solo(deck, file, { at = 1000, bpm = 120, play = true, musicalKey } = {}) {
    await h.load(deck, file, { bpm, musicalKey });
    await h.ok('mixer.channel.gain', { deck, gain: 1 });
    await h.ok('deck.seek', { deck, positionMs: at });
    if (play) await h.ok('deck.play', { deck });
  }
  const only = process.env.PLUMDECK_CONFORMANCE_ONLY?.split(',');
  async function check(id, title, refs, body) {
    if (only && !only.includes(id)) return;
    const entry = { id, title, refs, status: 'pass', metrics: {}, criteria: [] };
    report.checks.push(entry);
    await t.test(`${id} ${title}`, async () => {
      const metric = (name, value) => { entry.metrics[name] = typeof value === 'object' && value !== null ? JSON.parse(JSON.stringify(value, (_, v) => round(v))) : round(value); };
      const expect = (name, pass, detail) => { entry.criteria.push({ name, pass: Boolean(pass), detail: typeof detail === 'number' ? round(detail) : detail }); };
      try {
        await body({ metric, expect });
      } catch (error) {
        entry.status = error instanceof CommandError ? 'unsupported' : 'error';
        entry.error = error.message;
        throw error;
      } finally {
        try { await neutral(); } catch (error) { entry.cleanupError = error.message; }
      }
      const failed = entry.criteria.filter(c => !c.pass);
      if (failed.length) {
        entry.status = 'fail';
        assert.fail(failed.map(c => `${c.name}: ${JSON.stringify(c.detail)}`).join('; '));
      }
    });
  }
  const rejected = result => result.ok ? null : `rejected (${result.error?.code})`;
  /** Click copies of `template` after `fromS` seconds, as { ms, db } relative to the first. */
  function copies(recording, template = CLICK_A, fromS = 0, floorDb = -45, minGapMs = 10) {
    const start = Math.floor(fromS * RATE);
    const found = findCopies(recording.mono.subarray(start), template, { floorDb, minGap: Math.round(RATE * minGapMs / 1000) });
    const first = found[0];
    return found.map(c => ({ ms: ms((c.at + start) / RATE), gain: c.gain, db: first ? db(c.gain / first.gain) : 0 }));
  }

  /** First sample above -60 dBFS (s), used to map wall-clock marks onto the WAV. */
  const onset = recording => { const i = recording.mono.findIndex(v => Math.abs(v) > 1e-3); return i < 0 ? 0 : i / RATE; };
  const lastSound = recording => { let i = recording.mono.length - 1; while (i > 0 && Math.abs(recording.mono[i]) <= 1e-3) --i; return i / RATE; };

  await neutral();
  await h.ok('mixer.master.gain', { gain: 0.5 });

  // ---------------------------------------------------------------- QB-7 first
  // Must run first: it measures effect loads before anything is warm.
  await check('QB-7', 'first effect load and command round trip', ['QB-7', 'H-8'], async ({ metric, expect }) => {
    await h.load('A', files.sine);
    const first = {}, rejectedPads = [];
    const pads = [...PAD_BASE, ...(h.has('mixer.fx.beats') ? PAD_PROCESSORS : [])];
    for (const effect of pads) {
      const t0 = performance.now(), result = await pad('A', effect, true);
      if (result.ok) first[effect] = performance.now() - t0; else rejectedPads.push(effect);
      await pad('A', effect, false);
    }
    metric('padFirstPressMs', first);
    if (rejectedPads.length) metric('padRejected', rejectedPads);
    const beatFirst = {};
    for (const effect of BEAT_FX) {
      const t0 = performance.now(), result = await h.try('mixer.beatfx.set', { effect, target: 'A', enabled: true, mix: 0.5 });
      beatFirst[effect] = result.ok ? performance.now() - t0 : rejected(result);
      await h.try('mixer.beatfx.set', { enabled: false });
    }
    metric('beatFxFirstSwitchMs', beatFirst);
    const worstFirst = Math.max(...Object.values(first), ...Object.values(beatFirst).filter(v => typeof v === 'number'));
    expect('first effect load <= 100 ms', worstFirst <= 100, worstFirst);
    const trips = [];
    for (let i = 0; i < 60; ++i) {
      const t0 = performance.now();
      await pad('A', i % 2 ? 'echo' : 'flanger', i % 4 < 2);
      trips.push(performance.now() - t0);
    }
    trips.sort((a, b) => a - b);
    const p90 = trips[Math.floor(trips.length * 0.9)];
    metric('padRoundTripP50Ms', trips[trips.length >> 1]); metric('padRoundTripP90Ms', p90);
    expect('PAD FX round trip p90 <= 25 ms', p90 <= 25, p90);
  });

  // ---------------------------------------------------------------- QB-9
  await check('QB-9', 'effects stay silent on a stopped deck', ['QB-9'], async ({ metric, expect }) => {
    await solo('A', files.sine, { at: 0, play: false });
    const recording = await h.record(async mark => {
      for (const effect of BEAT_FX) {
        mark(`beat:${effect}`);
        await h.try('mixer.beatfx.set', { effect, target: 'A', enabled: true, mix: 1, beats: 0.5 });
        await delay(250);
      }
      await h.try('mixer.beatfx.set', { enabled: false });
      for (const effect of [...PAD_BASE, ...(h.has('mixer.fx.beats') ? PAD_PROCESSORS : [])]) {
        mark(`pad:${effect}`);
        await pad('A', effect, true, { mix: 1, depth: 0.8 });
        await delay(250);
        await pad('A', effect, false);
      }
      mark('end');
    }, 200);
    const names = Object.keys(recording.marks), loud = {};
    for (let i = 0; i < names.length - 1; ++i) {
      const value = db(peak(recording.mono, recording.marks[names[i]] * RATE, recording.marks[names[i + 1]] * RATE + RATE * 0.05));
      if (value > -90) loud[names[i]] = value;
    }
    const overall = db(peak(recording.mono));
    metric('peakDbfs', overall); metric('audibleSegments', loud);
    expect('stopped deck output <= -90 dBFS', overall <= -90, overall);
  });

  // ---------------------------------------------------------------- H-1
  await check('H-1', 'meters event: pre-fader calibration and rate', ['H-1', 'M-01'], async ({ metric, expect }) => {
    await solo('A', files.sine6);
    await h.ok('mixer.channel.gain', { deck: 'A', gain: 0 });
    const subscribed = await h.try('meters.subscribe', { enabled: true, intervalMs: 20 });
    metric('subscribe', subscribed.ok ? 'accepted' : rejected(subscribed));
    const before = h.meters.length;
    await delay(1000);
    const received = h.meters.slice(before);
    metric('eventsPerSecond', received.length);
    expect('meters update >= 30 Hz', received.length >= 30, received.length);
    const last = received.at(-1)?.data;
    const channelDb = last ? db(last.channels?.A?.peak ?? 0) : null;
    metric('channelPeakDbfs', channelDb); metric('masterPeakDbfs', last ? db(last.master?.peak ?? 0) : null);
    expect('-6 dBFS sine reads -6 +/- 0.5 dB with the fader closed', channelDb !== null && Math.abs(channelDb + 6.02) <= 0.5, channelDb);
    if (subscribed.ok) {
      await h.ok('meters.subscribe', { enabled: false });
      await delay(60);
      const stopped = h.meters.length; await delay(150);
      expect('unsubscribe stops events', h.meters.length === stopped, h.meters.length - stopped);
    }
  });

  // ---------------------------------------------------------------- H-2
  await check('H-2', 'syncLeader telemetry follows the engine', ['H-2', 'B-01'], async ({ metric, expect }) => {
    // Both decks audible: Mixxx lets only a playing, audible deck lead.
    await solo('A', files.pulse120); await solo('B', files.pulse128, { bpm: 128 });
    await h.ok('deck.sync.set', { deck: 'A', enabled: true, leader: 'A' });
    await h.ok('deck.sync.set', { deck: 'B', enabled: true });
    const waitLeader = async (predicate, limit = 1000) => {
      const t0 = performance.now();
      while (performance.now() - t0 < limit) { const s = await h.snapshot(); if (predicate(s)) return performance.now() - t0; await delay(5); }
      return null;
    };
    const initial = await waitLeader(s => s.decks.A.syncLeader === 'A' && s.decks.B.syncLeader === null);
    metric('leaderAVisibleMs', initial);
    // MASTER on B is the same op the UI sends: setSync(B, true, B).
    await h.ok('deck.sync.set', { deck: 'B', enabled: true, leader: 'B' });
    const moved = await waitLeader(s => s.decks.B.syncLeader === 'B' && s.decks.A.syncLeader === null);
    metric('masterBVisibleMs', moved);
    expect('MASTER on B visible on all decks within 100 ms', moved !== null && moved <= 100, moved);
    // The engine elects a new leader by itself when the leader leaves sync.
    await h.ok('deck.sync.set', { deck: 'B', enabled: false });
    const dropped = await waitLeader(s => s.decks.B.syncLeader !== 'B' && s.decks.A.syncLeader === 'A');
    const s = await h.snapshot();
    metric('afterLeaderOff', { A: s.decks.A.syncLeader, B: s.decks.B.syncLeader, ms: dropped });
    expect('engine-elected leader visible within 100 ms', dropped !== null && dropped <= 100, dropped);
  });

  // ---------------------------------------------------------------- H-3
  await check('H-3', 'BEAT SYNC one-shot phase alignment', ['H-3', 'S-01'], async ({ metric, expect }) => {
    // A: 120 BPM clicks (leader). B: distinct clicks, put 35% of a beat out of phase.
    const offsets = (recording, fromS) => {
      const tail = { ...recording, mono: recording.mono.subarray(Math.floor(fromS * RATE)) };
      // Real clicks correlate within -6 dB of the strongest; the other deck's
      // click only leaks in around -16 dB, even when both coincide.
      const a = copies(tail, CLICK_A, 0, -6, 100).map(c => c.ms), b = copies(tail, CLICK_B, 0, -6, 100).map(c => c.ms);
      const pairs = a.map(x => b.reduce((best, v) => Math.abs(v - x) < Math.abs(best - x) ? v : best, Infinity) - x)
        .filter(Number.isFinite).map(d => ((d % BEAT_MS) + BEAT_MS * 1.5) % BEAT_MS - BEAT_MS / 2);
      return { median: median(pairs), worst: pairs.length ? Math.max(...pairs.map(Math.abs)) : null, count: pairs.length };
    };
    const results = {};
    for (const [label, file, bpm] of [['B 120 -> A 120', files.beatB, 120], ['B 128 -> A 120', files.beatB128, 128]]) {
      await solo('A', files.beatA); await solo('B', file, { bpm });
      const beatB = 60000 / bpm, outOfPhase = async () => {
        const a = (await h.snapshot()).decks.A.positionMs;
        await h.ok('deck.seek', { deck: 'B', positionMs: (Math.floor(a / BEAT_MS) + 4.35) * beatB });
      };
      await outOfPhase();
      const synced = await h.record(async mark => {
        await delay(700);
        await h.ok('deck.sync.set', { deck: 'A', enabled: true, leader: 'A' });
        await h.ok('deck.sync.set', { deck: 'B', enabled: true }); mark('sync');
      }, 2200);
      const before = offsets({ ...synced, mono: synced.mono.subarray(0, Math.floor((synced.marks.sync - 0.1) * RATE)) }, 0);
      // Judge from one beat after SYNC ON (spec: aligned within one beat).
      const after = offsets(synced, synced.marks.sync + 0.5);
      // Starting playback while already synced must align as well.
      await h.ok('deck.pause', { deck: 'B' }); await outOfPhase();
      const started = await h.record(async mark => { await h.ok('deck.play', { deck: 'B' }); mark('play'); }, 2000);
      const onPlay = offsets(started, started.marks.play + 0.5);
      // From the very first block: B cued exactly on its beat while A is
      // mid-beat, so an unaligned pre-roll would sound a click ~half a beat off.
      await h.ok('deck.pause', { deck: 'B' });
      let a = (await h.snapshot()).decks.A.positionMs;
      await delay(((0.5 - (a % BEAT_MS) / BEAT_MS + 1) % 1) * BEAT_MS);
      a = (await h.snapshot()).decks.A.positionMs;
      await h.ok('deck.seek', { deck: 'B', positionMs: (Math.floor(a / BEAT_MS) + 8) * beatB });
      const cued = await h.record(async mark => { mark('play'); await h.ok('deck.play', { deck: 'B' }); }, 900);
      const from = Math.max(0, cued.marks.play - 0.05);
      const firstWindow = { ...cued, mono: cued.mono.subarray(0, Math.floor((cued.marks.play + 0.6) * RATE)) };
      const firstClicks = offsets(firstWindow, from);
      results[label] = { before, afterSyncOn: after, afterSyncedPlay: onPlay, firstBlocksAfterPlay: firstClicks };
      expect(`${label}: no unaligned click in the first 600 ms after play`, firstClicks.count >= 1 && firstClicks.worst <= 2.5, firstClicks);
      expect(`${label}: decks start out of phase`, before.count > 0 && before.worst > 50, before);
      expect(`${label}: phase error <= 2 ms one beat after SYNC ON`, after.count >= 2 && Math.abs(after.median) <= 2 && after.worst <= 2.5, after);
      expect(`${label}: phase error <= 2 ms after play while synced`, onPlay.count >= 2 && Math.abs(onPlay.median) <= 2 && onPlay.worst <= 2.5, onPlay);
      await neutral();
    }
    metric('phase', results);
  });

  // ---------------------------------------------------------------- H-4 / H-5
  await check('H-4', 'KEY SYNC uses the loaded key', ['H-4', 'B-02'], async ({ metric, expect }) => {
    await solo('A', files.sine, { musicalKey: 'Am' }); await h.ok('mixer.channel.gain', { deck: 'A', gain: 0 });
    await solo('B', files.sine, { musicalKey: 'Cm' });
    await h.ok('deck.sync.set', { deck: 'A', enabled: true, leader: 'A' });
    const synced = await h.try('deck.key.sync', { deck: 'B' });
    metric('keySync', synced.ok ? 'accepted' : rejected(synced));
    let state = await h.snapshot();
    for (let i = 0; i < 50 && state.decks.B.keyShift !== -3; ++i) { await delay(20); state = await h.snapshot(); }
    metric('keyShift', state.decks.B.keyShift); metric('musicalKey', { A: state.decks.A.musicalKey, B: state.decks.B.musicalKey });
    expect('B (Cm) KEY SYNC to A (Am) is -3 semitones', state.decks.B.keyShift === -3, state.decks.B.keyShift);
    const recording = await h.record(null, 900);
    const hz = dominantFrequency(recording.mono, RATE, 700, 1200, RATE * 0.1, RATE * 0.8);
    const cents = 1200 * Math.log2(hz / (1000 * 2 ** (-3 / 12)));
    metric('toneHz', hz); metric('centsFrom840.9', cents);
    expect('1 kHz tone becomes 840.9 Hz +/- 3 cents', Math.abs(cents) <= 3, hz);
  });
  await check('H-5', 'KEY RESET restores the previous MT state', ['H-5'], async ({ metric, expect }) => {
    await solo('A', files.sine, { play: false });
    const states = [];
    for (const initial of [false, true]) {
      await h.ok('deck.keylock.set', { deck: 'A', enabled: initial });
      await h.ok('deck.key.shift', { deck: 'A', semitones: 2 });
      const during = (await h.until(s => s.decks.A.keyShift === 2, 'key shift')).decks.A.keylock;
      await h.ok('deck.key.reset', { deck: 'A' });
      const after = await h.until(s => s.decks.A.keyShift === 0, 'key reset');
      await delay(60);
      const final = (await h.snapshot()).decks.A.keylock;
      states.push({ initial, during, after: final });
      expect(`MT ${initial} -> shift -> reset restores ${initial}`, final === initial, { during, after: final });
      void after;
    }
    metric('keylock', states);
  });

  // ---------------------------------------------------------------- H-6
  await check('H-6', 'SLIP returns after loop, reverse and scratch', ['H-6'], async ({ metric, expect }) => {
    // B plays the same file unheard as the running reference timeline.
    await solo('A', files.pulse120, { play: false }); await solo('B', files.pulse120, { play: false });
    await h.ok('mixer.channel.gain', { deck: 'B', gain: 0 });
    // SLIP is armed while stopped, as a DJ would before starting the deck.
    await h.ok('deck.slip.set', { deck: 'A', enabled: true });
    await h.ok('deck.play', { deck: 'A' }); await h.ok('deck.play', { deck: 'B' });
    const gap = s => s.decks.A.positionMs - s.decks.B.positionMs;
    const results = {};
    const measure = async (name, gesture) => {
      await delay(150);
      const before = gap(await h.snapshot());
      await gesture();
      await delay(150);
      const after = await h.snapshot();
      results[name] = { errorMs: Math.abs(gap(after) - before), slipArmed: after.decks.A.slip };
      expect(`${name}: back on the slip timeline within 25 ms`, results[name].errorMs <= 25, results[name].errorMs);
    };
    await measure('loop', async () => {
      await h.ok('deck.loop.beats', { deck: 'A', beats: 0.25 }); await delay(400);
      await h.ok('deck.loop.enable', { deck: 'A', enabled: false });
    });
    await measure('reverse', async () => {
      await h.ok('deck.reverse.set', { deck: 'A', enabled: true }); await delay(300);
      await h.ok('deck.reverse.set', { deck: 'A', enabled: false });
    });
    await measure('scratch', async () => {
      const gestureId = `slip-${Date.now()}`;
      await h.ok('deck.scratch', { deck: 'A', phase: 'begin', gestureId, positionMs: 0 });
      for (let i = 1; i <= 15; ++i) { await h.ok('deck.scratch', { deck: 'A', phase: 'move', gestureId, positionMs: -10 * i }); await delay(20); }
      await h.ok('deck.scratch', { deck: 'A', phase: 'end', gestureId, positionMs: -150 });
      await delay(400);
    });
    // Tempo changed after SLIP was armed: the slip timeline must follow the
    // new speed (Mixxx fixes it when slip engages, so it may only engage per gesture).
    await h.ok('deck.tempo.set', { deck: 'A', rate: 1.06 }); await h.ok('deck.tempo.set', { deck: 'B', rate: 1.06 });
    await delay(2000);
    await measure('loop after tempo change', async () => {
      await h.ok('deck.loop.beats', { deck: 'A', beats: 0.25 }); await delay(400);
      await h.ok('deck.loop.enable', { deck: 'A', enabled: false });
    });
    // Pausing with SLIP armed and no gesture must not move the playhead.
    const playing = await h.snapshot();
    await h.ok('deck.pause', { deck: 'A' }); await h.ok('deck.pause', { deck: 'B' });
    await delay(150);
    const paused = await h.snapshot();
    const pauseJumpMs = gap(paused) - gap(playing);
    results.pause = { jumpMs: pauseJumpMs, slipArmed: paused.decks.A.slip };
    expect('pause with SLIP armed after a tempo change: no jump (<= 25 ms)', Math.abs(pauseJumpMs) <= 25, pauseJumpMs);
    metric('slip', results);
    expect('SLIP stays armed', Object.values(results).every(r => r.slipArmed), Object.fromEntries(Object.entries(results).map(([k, v]) => [k, v.slipArmed])));
  });

  // ---------------------------------------------------------------- H-7
  await check('H-7', 'headphone LEVEL/MIX ops and PFL wording', ['H-7', 'M-03'], async ({ metric, expect }) => {
    const gain = await h.try('mixer.headphone.gain', { gain: 0.4 }), mix = await h.try('mixer.headphone.mix', { mix: 0.25 });
    const s = await h.snapshot();
    metric('ops', { gain: gain.ok ? 'accepted' : rejected(gain), mix: mix.ok ? 'accepted' : rejected(mix) });
    metric('telemetry', { headphoneGain: s.mixer.headphoneGain, headphoneMix: s.mixer.headphoneMix });
    expect('headphone gain/mix accepted and reported', gain.ok && mix.ok && s.mixer.headphoneGain === 0.4 && s.mixer.headphoneMix === 0.25, s.mixer.headphoneGain);
    const range = await h.try('mixer.headphone.mix', { mix: 1.5 });
    expect('out-of-range mix rejected', !range.ok && range.error?.code === 'invalid_params', range.error?.code);
    const pfl = await h.try('mixer.channel.pfl', { deck: 'A', enabled: true });
    metric('pflOn2chDevice', pfl.ok ? 'accepted' : pfl.error?.message);
    expect('PFL refusal names no controller model', pfl.ok || !/DDJ/i.test(pfl.error?.message ?? ''), pfl.error?.message);
    await h.try('mixer.channel.pfl', { deck: 'A', enabled: false });
  });

  // ---------------------------------------------------------------- H-11
  await check('H-11', 'loop 1/64..512 beats and beat jump 128', ['H-11'], async ({ metric, expect }) => {
    await solo('A', files.pulse120, { at: 2050, play: false });
    const short = await h.try('deck.loop.beats', { deck: 'A', beats: 1 / 64 });
    let s = await h.snapshot();
    const shortMs = s.decks.A.loopRegion ? s.decks.A.loopRegion.endMs - s.decks.A.loopRegion.startMs : null;
    metric('loop1/64', short.ok ? shortMs : rejected(short));
    expect('1/64-beat loop is 7.8125 +/- 0.1 ms', short.ok && Math.abs(shortMs - 7.8125) <= 0.1, short.ok ? shortMs : rejected(short));
    if (short.ok) {
      await h.ok('deck.play', { deck: 'A' });
      const recording = await h.record(null, 600);
      const { period } = periodByAutocorrelation(recording.mono.subarray(RATE * 0.2, RATE * 0.6), RATE, 0.005, 0.012);
      metric('loop1/64AudioPeriodMs', ms(period));
      expect('1/64-beat loop audio period 7.8125 +/- 0.1 ms', Math.abs(ms(period) - 7.8125) <= 0.1, ms(period));
      await h.ok('deck.pause', { deck: 'A' });
    }
    await h.try('deck.loop.enable', { deck: 'A', enabled: false });
    await h.ok('deck.seek', { deck: 'A', positionMs: 1000 });
    await h.until(st => Math.abs(st.decks.A.positionMs - 1000) < 1, 'seek');
    const long = await h.try('deck.loop.beats', { deck: 'A', beats: 512 });
    s = await h.snapshot();
    const longMs = long.ok && s.decks.A.loopRegion ? s.decks.A.loopRegion.endMs - s.decks.A.loopRegion.startMs : null;
    metric('loop512', long.ok ? longMs : rejected(long));
    expect('512-beat loop is 256000 ms', long.ok && Math.abs(longMs - 256000) <= 0.1, long.ok ? longMs : rejected(long));
    await h.try('deck.loop.enable', { deck: 'A', enabled: false });
    const jump = await h.try('deck.beatjump', { deck: 'A', beats: 128 });
    s = jump.ok ? await h.until(st => st.decks.A.positionMs > 60000, '128-beat jump', 2000).catch(() => h.snapshot()) : s;
    metric('beatjump128', jump.ok ? s.decks.A.positionMs - 1000 : rejected(jump));
    expect('+128 beats moves +64000 ms', jump.ok && Math.abs(s.decks.A.positionMs - 65000) <= 1, jump.ok ? s.decks.A.positionMs : rejected(jump));
    const tooLong = await h.try('deck.loop.beats', { deck: 'A', beats: 513 });
    expect('513 beats rejected', !tooLong.ok, tooLong.ok);
  });

  // ---------------------------------------------------------------- H-12
  await check('H-12', 'crossfader curves', ['H-12', 'M-05'], async ({ metric, expect }) => {
    await solo('A', files.sine);
    await h.ok('mixer.channel.orientation', { deck: 'A', orientation: 0 });
    const positions = [-1, -0.5, 0, 0.5, 0.9, 1];
    const table = {};
    // The startup curve must keep what hosts before H-12 played: Mixxx's
    // unconfigured profile, i.e. both sides full at the centre (c78cb94).
    const reported = (await h.snapshot()).mixer.crossfaderCurve ?? null;
    metric('startupCurve', reported);
    expect('host reports sharp at startup', reported === 'sharp', reported);
    for (const curve of ['startup', 'smooth', 'standard', 'sharp']) {
      const applied = curve === 'startup' ? { ok: true } : await h.try('mixer.crossfader.curve', { curve });
      if (!applied.ok && curve !== 'standard') { table[curve] = rejected(applied); expect(`${curve} curve accepted`, false, rejected(applied)); continue; }
      const telemetry = [];
      await h.ok('deck.pause', { deck: 'A' });
      await h.ok('mixer.crossfader', { position: -1 });
      const recording = await h.record(async mark => {
        await h.ok('deck.play', { deck: 'A' }); mark('play'); await delay(200);
        for (const position of positions) {
          await h.ok('mixer.crossfader', { position }); mark(String(position));
          telemetry.push((await h.snapshot()).mixer.channels.A.crossfaderGain ?? null);
          await delay(350);
        }
      }, 100);
      const shift = onset(recording) - recording.marks.play;
      const level = positions.map(p => toneAmplitude(recording.mono, RATE, 1000, (recording.marks[String(p)] + shift + 0.08) * RATE, (recording.marks[String(p)] + shift + 0.3) * RATE));
      const rows = positions.map((p, i) => ({ position: p, db: Math.max(-120, db(level[i] / level[0])), telemetryDb: telemetry[i] === null ? null : Math.max(-120, db(telemetry[i] / (telemetry[0] || 1))) }));
      table[curve] = applied.ok ? rows : { note: `curve op ${rejected(applied)}; Mixxx default`, rows };
      const mismatch = rows.filter(r => r.telemetryDb !== null && r.telemetryDb > -60 && Math.abs(r.db - r.telemetryDb) > 0.5);
      if (applied.ok) expect(`${curve}: audio matches reported gain +/- 0.5 dB`, rows.every(r => r.telemetryDb !== null) && mismatch.length === 0, mismatch);
      if (curve === 'smooth' && applied.ok) expect('smooth: centre -3 +/- 0.5 dB', Math.abs(rows[2].db + 3.01) <= 0.5, rows[2].db);
      if (curve === 'sharp' && applied.ok) expect('sharp: +0.5 still within 0.5 dB of full', rows[3].db >= -0.5, rows[3].db);
      if (curve === 'standard' && applied.ok) expect('standard: centre at full level +/- 0.5 dB', Math.abs(rows[2].db) <= 0.5, rows[2].db);
      if (curve === 'startup') expect('startup: like c78cb94, centre and +0.5 full, +0.9 within 0.5 dB, +1 cut', Math.abs(rows[2].db) <= 0.5 && rows[3].db >= -0.5 && rows[4].db >= -0.5 && rows[5].db <= -60, rows.map(r => r.db));
    }
    metric('curves', table);
    await h.try('mixer.crossfader.curve', { curve: 'sharp' });
  });

  // ---------------------------------------------------------------- FX timing
  /** Ends running echo/reverb tails, so each take starts from an empty delay. */
  async function flushTails() {
    await h.try('mixer.beatfx.set', { effect: 'flanger', target: 'A', enabled: false });
    await pad('A', 'flanger', true); await pad('A', 'flanger', false);
  }
  async function echoTake(configure, { beats = 1, lengthMs = 1300 } = {}) {
    // A click at 4000 ms; the recording starts ~100 ms before it.
    await h.ok('deck.pause', { deck: 'A' }); await h.ok('deck.seek', { deck: 'A', positionMs: 3850 });
    await flushTails();
    await configure();
    const recording = await h.record(async () => { await h.ok('deck.play', { deck: 'A' }); }, Math.max(lengthMs, ms(beats * 0.5) + 700));
    return copies(recording);
  }
  /**
   * Echo delay from one click. Each repeat after the direct (strongest) click
   * is a candidate delay; the one leaving the fewest audible copies off its
   * grid wins (ties: the louder repeat). The leftovers are stale audio
   * replayed from an earlier setting.
   */
  function echoDelay(found) {
    if (!found.length) return { delayMs: null, offGrid: 0 };
    const direct = found.reduce((a, b) => (b.gain > a.gain ? b : a));
    const audible = found.filter(c => c !== direct && db(c.gain / direct.gain) > -40);
    const offGridFor = delay => audible.filter(c => { const k = (c.ms - direct.ms) / delay; return k < 0.5 || Math.abs(k - Math.round(k)) * delay > 2; }).length;
    let best = { delayMs: null, offGrid: audible.length, gain: 0 };
    for (const c of audible.filter(c => c.ms > direct.ms + 5)) {
      const delayMs = c.ms - direct.ms, offGrid = offGridFor(delayMs);
      if (offGrid < best.offGrid || (offGrid === best.offGrid && c.gain > best.gain)) best = { delayMs, offGrid, gain: c.gain };
    }
    return { delayMs: best.delayMs, offGrid: best.offGrid };
  }
  await check('QB-1', 'beat-synced periods (BEAT FX / PAD echo, TRANS)', ['QB-1', 'H-8', 'H-10', 'X-05'], async ({ metric, expect }) => {
    await solo('A', files.clicks, { play: false });
    const beatFx = {};
    for (const [beats, bpm, expected] of [[1 / 16, null, 31.25], [0.25, null, 125], [0.5, null, 250], [1, null, 500], [4, null, 2000], [0.5, 100, 300], [1, 100, 600]]) {
      const key = `${bpm ? `TAP${bpm} ` : ''}${beats}`;
      let accepted;
      const found = await echoTake(async () => {
        accepted = await h.try('mixer.beatfx.set', { effect: 'echo', target: 'A', enabled: true, mix: 0.7, beats, ...(bpm ? { bpm } : { auto: true }) });
      }, { beats: bpm ? beats * 120 / bpm : beats });
      await h.try('mixer.beatfx.set', { enabled: false, auto: true });
      const { delayMs, offGrid } = echoDelay(found);
      beatFx[key] = accepted.ok ? { delayMs, offGrid } : rejected(accepted);
      expect(`BEAT FX ECHO ${key} beat = ${expected} +/- 1 ms`, accepted.ok && delayMs !== null && Math.abs(delayMs - expected) <= 1, accepted.ok ? delayMs : beatFx[key]);
      if (accepted.ok) expect(`BEAT FX ECHO ${key}: no stale repeats off the beat grid`, offGrid === 0, offGrid);
    }
    metric('beatFxEchoDelayMs', beatFx);
    const padEcho = {};
    for (const [beats, expected] of [[0.5, 250], [0.75, 375], [1, 500]]) {
      let accepted;
      const found = await echoTake(async () => { accepted = await pad('A', 'echo', true, { mix: 0.5, depth: 0.7, beats }); }, { beats });
      await pad('A', 'echo', false);
      const { delayMs, offGrid } = echoDelay(found);
      padEcho[beats] = accepted.ok ? { delayMs, offGrid } : rejected(accepted);
      expect(`PAD ECHO ${beats} beat = ${expected} +/- 1 ms`, accepted.ok && delayMs !== null && Math.abs(delayMs - expected) <= 1, accepted.ok ? delayMs : padEcho[beats]);
      if (accepted.ok) expect(`PAD ECHO ${beats}: no stale repeats off the beat grid`, offGrid === 0, offGrid);
    }
    metric('padEchoDelayMs', padEcho);
  });

  await check('QB-2', 'TRANS gate period and phase against the beat grid', ['QB-1', 'QB-2', 'X-07'], async ({ metric, expect }) => {
    await solo('A', files.grid, { at: 1000 });
    const takes = {};
    for (const [label, on, off] of [
      ['BEAT FX TRANS 1/4', () => h.try('mixer.beatfx.set', { effect: 'tremolo', target: 'A', enabled: true, mix: 1, beats: 0.25, auto: true }), () => h.try('mixer.beatfx.set', { enabled: false })],
      ['PAD TRANS 1/4', () => pad('A', 'tremolo', true, { mix: 1, depth: 1, beats: 0.25 }), () => pad('A', 'tremolo', false)],
    ]) {
      const accepted = await on();
      const recording = await h.record(null, 1600);
      await off();
      if (!accepted.ok) { takes[label] = rejected(accepted); expect(`${label} accepted`, false, takes[label]); continue; }
      const clicks = copies(recording, CLICK_A, 0.1, -12, 100).map(c => c.ms);
      // Remove the clicks before reading the 1 kHz gate envelope.
      const tone = Float64Array.from(recording.mono);
      for (const c of clicks) tone.fill(0, Math.floor(c / 1000 * RATE) - 10, Math.floor(c / 1000 * RATE) + 60);
      // The gate opens where the envelope first rises through 10% of its range.
      const { edges, contrastDb } = risingEdges(tone.subarray(Math.floor(0.1 * RATE)), RATE, { smoothMs: 1, minGapMs: 60, level: 0.1 });
      const edgeMs = edges.map(e => ms(e) + 100);
      const period = median(edgeMs.slice(1).map((e, i) => e - edgeMs[i]));
      const grid = clicks.length ? clicks[0] : 0, sub = BEAT_MS / 4;
      const phase = median(edgeMs.map(e => ((e - grid) % sub + sub * 1.5) % sub - sub / 2));
      takes[label] = { periodMs: period, phaseMs: phase, contrastDb, edges: edgeMs.length };
      expect(`${label}: period 125 +/- 1 ms`, Math.abs(period - 125) <= 1, period);
      expect(`${label}: gate edge within 3 ms of the grid`, Math.abs(phase) <= 3, phase);
    }
    metric('trans', takes);
  });

  // ---------------------------------------------------------------- QB-3
  await check('QB-3', 'dry preservation and insert leakage', ['QB-3', 'H-8', 'H-10', 'X-01', 'X-02'], async ({ metric, expect }) => {
    await solo('A', files.clicks, { play: false });
    const direct = found => found.reduce((a, b) => (b.gain > a.gain ? b : a), { gain: 0 }).gain;
    const dryGain = direct(await echoTake(async () => {}));
    const dry = {};
    for (const effect of ['echo', 'lowcutecho', 'mtdelay', 'spiral', 'reverb']) {
      for (const mix of [0.5, 1]) {
        let accepted;
        const found = await echoTake(async () => { accepted = await h.try('mixer.beatfx.set', { effect, target: 'A', enabled: true, mix, beats: 1, auto: true }); });
        await h.try('mixer.beatfx.set', { enabled: false });
        const change = found.length ? db(direct(found) / dryGain) : -120;
        dry[`BEAT ${effect} ${mix}`] = accepted.ok ? change : rejected(accepted);
        expect(`BEAT FX ${effect} LEVEL ${mix}: dry within +/- 1 dB`, accepted.ok && Math.abs(change) <= 1, dry[`BEAT ${effect} ${mix}`]);
      }
    }
    for (const effect of ['echo', 'reverb']) {
      for (const mix of [0.5, 1]) {
        let accepted;
        const found = await echoTake(async () => { accepted = await pad('A', effect, true, { mix, depth: 0.7, ...(h.has('mixer.fx.beats') && effect === 'echo' ? { beats: 1 } : {}) }); });
        await pad('A', effect, false);
        const change = found.length ? db(direct(found) / dryGain) : -120;
        dry[`PAD ${effect} ${mix}`] = accepted.ok ? change : rejected(accepted);
        expect(`PAD ${effect} mix ${mix}: dry within +/- 1 dB`, accepted.ok && Math.abs(change) <= 1, dry[`PAD ${effect} ${mix}`]);
      }
    }
    metric('directClickChangeDb', dry);

    // LEVEL 0 must be the dry signal.
    await h.load('A', files.pulse120);
    const take = async configure => {
      await h.ok('deck.pause', { deck: 'A' }); await h.ok('deck.seek', { deck: 'A', positionMs: 2000 }); await flushTails(); await configure();
      return h.record(async () => { await h.ok('deck.play', { deck: 'A' }); }, 1300);
    };
    const reference = await take(async () => {});
    const level0 = {};
    for (const [label, on, off] of [
      ['BEAT echo LEVEL 0', () => h.try('mixer.beatfx.set', { effect: 'echo', target: 'A', enabled: true, mix: 0, beats: 0.5, auto: true }), () => h.try('mixer.beatfx.set', { enabled: false })],
      ['PAD echo mix 0', () => pad('A', 'echo', true, { mix: 0, depth: 0.7 }), () => pad('A', 'echo', false)],
    ]) {
      const recording = await take(on); await off();
      level0[label] = residualDb(recording.mono, reference.mono, RATE * 0.4, RATE * 1.2);
      expect(`${label}: residual vs dry <= -60 dB`, level0[label] <= -60, level0[label]);
    }
    metric('level0ResidualDb', level0);

    // Insert effects: filter extremes on the 80 Hz / 8 kHz bursts.
    const bands = recording => Object.fromEntries([80, 1000, 8000].map(hz => {
      let best = 0;
      for (let s = 0.2; s + 0.19 < recording.frames / RATE; s += 0.05) best = Math.max(best, toneAmplitude(recording.mono, RATE, hz, s * RATE, (s + 0.19) * RATE));
      return [hz, best];
    }));
    const dryBands = bands(reference), filter = {};
    for (const depth of [0, 1]) {
      const recording = await take(() => pad('A', 'filter', true, { mix: 0.3, depth })); await pad('A', 'filter', false);
      const b = bands(recording);
      filter[`depth ${depth}`] = { '80Hz': db(b[80] / dryBands[80]), '8kHz': db(b[8000] / dryBands[8000]) };
    }
    metric('padFilterBandChangeDb', filter);
    const lpf = Math.min(...Object.values(filter).map(v => v['8kHz'])), hpf = Math.min(...Object.values(filter).map(v => v['80Hz']));
    expect('PAD FILTER reaches LPF: 8 kHz <= -40 dB', lpf <= -40, lpf);
    expect('PAD FILTER reaches HPF: 80 Hz <= -40 dB', hpf <= -40, hpf);

    // PITCH is an insert: the unshifted tone must not leak at low LEVEL.
    await h.load('A', files.sine);
    const sineRef = await take(async () => {});
    const refAmp = toneAmplitude(sineRef.mono, RATE, 1000, RATE * 0.3, RATE * 1.1), pitch = {};
    for (const mix of [0, 0.25]) {
      const recording = await take(() => h.try('mixer.beatfx.set', { effect: 'pitchshift', target: 'A', enabled: true, mix, auto: true }));
      await h.try('mixer.beatfx.set', { enabled: false });
      const leak = db(toneAmplitude(recording.mono, RATE, 1000, RATE * 0.3, RATE * 1.1) / refAmp);
      pitch[`LEVEL ${mix}`] = { leakDb: leak, hz: dominantFrequency(recording.mono, RATE, 100, 2500, RATE * 0.3, RATE * 1.1) };
      expect(`BEAT FX PITCH LEVEL ${mix}: 1 kHz leak <= -40 dB`, leak <= -40, leak);
    }
    metric('pitch', pitch);
  });

  // ---------------------------------------------------------------- QB-4
  await check('QB-4', 'echo and reverb tails after release / OFF', ['QB-4', 'H-8', 'X-04'], async ({ metric, expect }) => {
    await solo('A', files.clicks, { play: false });
    const tails = {};
    const takeTail = async (label, on, off, kind) => {
      await h.ok('deck.pause', { deck: 'A' }); await h.ok('deck.seek', { deck: 'A', positionMs: 3850 });
      await flushTails();
      const accepted = await on();
      const recording = await h.record(async mark => {
        await h.ok('deck.play', { deck: 'A' }); mark('play');
        await delay(kind === 'echo' ? 500 : 400); await off(); mark('release');
      }, 2600);
      // The first click is 150 ms into playback; use it to place the release on the WAV.
      const shift = onset(recording) - 0.15 - recording.marks.play;
      if (!accepted.ok) { tails[label] = rejected(accepted); expect(`${label} accepted`, false, tails[label]); return; }
      const release = recording.marks.release + shift;
      if (kind === 'echo') {
        const found = copies(recording, CLICK_A, 0, -60, 20);
        const after = found.filter(c => c.ms / 1000 > release && c.db > -40);
        const span = after.length ? after.at(-1).ms - ms(release) : 0;
        tails[label] = { spanMs: span, repeatsAfterRelease: after.length };
        expect(`${label}: echo audible (> -40 dB) >= 2 beats after release`, span >= 2 * BEAT_MS - 250, span);
      } else {
        const env = envelope(recording.mono, RATE, 20), start = Math.ceil(release * 50);
        const top = Math.max(...env);
        let end = start;
        while (end < env.length && db(env[end] / top) > -60) ++end;
        let rises = 0;
        for (let i = start + 2; i < end; ++i) if (db(env[i] / env[i - 2]) > 3) ++rises;
        tails[label] = { toMinus60Ms: (end - start) * 20, rises };
        expect(`${label}: reverb decays for >= 300 ms after release`, (end - start) * 20 >= 300, (end - start) * 20);
        expect(`${label}: decay is monotonic`, rises === 0, rises);
      }
    };
    const padBeats = h.has('mixer.fx.beats') ? { beats: 0.5 } : {};
    await takeTail('PAD echo release', () => pad('A', 'echo', true, { mix: 0.5, depth: 0.7, ...padBeats }), () => pad('A', 'echo', false), 'echo');
    await takeTail('BEAT FX echo OFF', () => h.try('mixer.beatfx.set', { effect: 'echo', target: 'A', enabled: true, mix: 0.7, beats: 0.5, auto: true }), () => h.try('mixer.beatfx.set', { enabled: false }), 'echo');
    await takeTail('PAD reverb release', () => pad('A', 'reverb', true, { mix: 0.8, depth: 0.8 }), () => pad('A', 'reverb', false), 'reverb');
    await takeTail('BEAT FX reverb OFF', () => h.try('mixer.beatfx.set', { effect: 'reverb', target: 'A', enabled: true, mix: 0.8, auto: true }), () => h.try('mixer.beatfx.set', { enabled: false }), 'reverb');
    metric('tails', tails);
  });

  // ---------------------------------------------------------------- QB-5
  await check('QB-5', 'no clicks when effects switch', ['QB-5'], async ({ metric, expect }) => {
    await solo('A', files.sine);
    const clicks = {};
    const baseline = await h.record(null, 1200);
    // Skip the take's own start and end, where the WAV cuts into the tone.
    const span = recording => ({ from: (onset(recording) + 0.03) * RATE, to: (lastSound(recording) - 0.03) * RATE });
    clicks.none = worstOutOfBandDb(baseline.mono, RATE, 1000, span(baseline));
    for (const [label, on, off] of [
      ['PAD filter', () => pad('A', 'filter', true, { mix: 1, depth: 0.25 }), () => pad('A', 'filter', false)],
      ['PAD echo', () => pad('A', 'echo', true, { mix: 0.5, depth: 0.7 }), () => pad('A', 'echo', false)],
      ['BEAT FX echo', () => h.try('mixer.beatfx.set', { effect: 'echo', target: 'A', enabled: true, mix: 0.7, beats: 0.5, auto: true }), () => h.try('mixer.beatfx.set', { enabled: false })],
      ['BEAT FX flanger', () => h.try('mixer.beatfx.set', { effect: 'flanger', target: 'A', enabled: true, mix: 0.7, beats: 4, auto: true }), () => h.try('mixer.beatfx.set', { enabled: false })],
    ]) {
      const recording = await h.record(async () => {
        for (let i = 0; i < 3; ++i) { await on(); await delay(250); await off(); await delay(250); }
      }, 400);
      clicks[label] = worstOutOfBandDb(recording.mono, RATE, 1000, span(recording));
      expect(`${label} ON/OFF: out-of-band burst <= -50 dB`, clicks[label] <= -50, clicks[label]);
    }
    metric('worstOutOfBandDb', clicks);
  });

  // ---------------------------------------------------------------- QB-8
  await check('QB-8', 'RELEASE FX returns to dry', ['QB-8', 'X-04'], async ({ metric, expect }) => {
    await solo('A', files.sine);
    const reference = await h.record(null, 900);
    const refAmp = toneAmplitude(reference.mono, RATE, 1000, RATE * 0.2, RATE * 0.8);
    await h.ok('mixer.beatfx.set', { effect: 'flanger', target: 'A', enabled: true, mix: 0.8, beats: 4, auto: true });
    const recording = await h.record(async mark => {
      await delay(300);
      const on = await h.try('mixer.beatfx.set', { release: true }); mark('on'); void on;
      await delay(600);
      await h.try('mixer.beatfx.set', { release: false }); mark('off');
    }, 1300);
    const back = db(toneAmplitude(recording.mono, RATE, 1000, (recording.marks.off + 0.5) * RATE, (recording.marks.off + 1.1) * RATE) / refAmp);
    const state = (await h.snapshot()).mixer.beatFx;
    metric('dryAfterOneBeatDb', back); metric('stateAfter', { effect: state?.effect, enabled: state?.enabled, releaseActive: state?.releaseActive });
    expect('dry back to 0 +/- 0.5 dB one beat after release', Math.abs(back) <= 0.5, back);
  });

  report.notRun = { 'QB-6': 'three simultaneous PAD FX need H-9 (optional, not implemented on the Mixxx host)' };
});
