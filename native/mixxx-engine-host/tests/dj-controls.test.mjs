import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';

const binary = process.env.PLUMDECK_TEST_HOST || path.resolve(import.meta.dirname, '../build-upstream/plumdeck-mixxx-engine-host');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
function fixture() {
  const rate = 44100, frames = rate * 24;
  const data = Buffer.alloc(44 + frames * 4);
  data.write('RIFF'); data.writeUInt32LE(data.length - 8, 4); data.write('WAVEfmt ', 8);
  data.writeUInt32LE(16, 16); data.writeUInt16LE(1, 20); data.writeUInt16LE(2, 22);
  data.writeUInt32LE(rate, 24); data.writeUInt32LE(rate * 4, 28);
  data.writeUInt16LE(4, 32); data.writeUInt16LE(16, 34); data.write('data', 36); data.writeUInt32LE(frames * 4, 40);
  for (let n = 0; n < frames; n++) {
    // Separated low, middle and high bands; amplitude envelope exposes FX tails.
    const envelope = n % rate < rate * 0.65 ? 1 : 0;
    const value = Math.round(envelope * 2200 * [80, 1000, 8000].reduce((sum, hz) => sum + Math.sin(2 * Math.PI * hz * n / rate), 0));
    data.writeInt16LE(value, 44 + n * 4); data.writeInt16LE(value, 46 + n * 4);
  }
  return data;
}
function amplitudes(wav) {
  let pcm, rate, bits;
  for (let offset = 12; offset + 8 <= wav.length;) {
    const name = wav.toString('ascii', offset, offset + 4), size = wav.readUInt32LE(offset + 4);
    if (name === 'fmt ') { rate = wav.readUInt32LE(offset + 12); bits = wav.readUInt16LE(offset + 22); }
    if (name === 'data') pcm = wav.subarray(offset + 8, offset + 8 + size);
    offset += 8 + size + size % 2;
  }
  assert.equal(bits, 16); assert(pcm?.length > 1000, `Finalized WAV PCM required (${wav.length} bytes; header ${wav.subarray(0, 100).toString('hex')})`);
  const width = 8192, result = [0, 0, 0];
  result.minimumRms = Infinity;
  for (let start = Math.floor(rate * 0.2); start + 1024 < pcm.length / 4; start += 1024) {
    let sum = 0;
    for (let n = 0; n < 1024; n++) sum += pcm.readInt16LE((start + n) * 4) ** 2;
    result.minimumRms = Math.min(result.minimumRms, Math.sqrt(sum / 1024));
  }
  // Find the strongest sustained tone window, ignoring recording startup ramps.
  for (let start = Math.floor(rate * 0.2); start + width < pcm.length / 4; start += width) {
    [80, 1000, 8000].forEach((hz, band) => {
      let real = 0, imaginary = 0;
      for (let n = 0; n < width; n++) {
        const sample = pcm.readInt16LE((start + n) * 4), phase = 2 * Math.PI * hz * n / rate;
        real += sample * Math.cos(phase); imaginary += sample * Math.sin(phase);
      }
      result[band] = Math.max(result[band], 2 * Math.hypot(real, imaginary) / width);
    });
  }
  return result;
}

test('four decks use native cues, variable-grid jumps/loops, EQ/filter/trim and FX', { timeout: 90000 }, async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'plumdeck-controls-'));
  const audioPath = path.join(directory, 'tones.wav');
  await writeFile(audioPath, fixture());
  const child = spawn(binary, [], { env: { ...process.env, PLUMDECK_MIXXX_OUTPUT_DEVICE: process.env.PLUMDECK_MIXXX_OUTPUT_DEVICE || 'BlackHole 2ch', PLUMDECK_MIXXX_RECORDING_DIR: directory } });
  let stderr = '', nextId = 0, hello;
  const pending = new Map();
  child.stderr.on('data', chunk => { stderr += chunk; });
  const exited = new Promise(resolve => child.once('exit', (code, signal) => resolve({ code, signal })));
  createInterface({ input: child.stdout }).on('line', line => { const value = JSON.parse(line); if (value.kind !== 'event') pending.get(value.id)?.(value); });
  async function command(op, params = {}, errorCode) {
    const id = ++nextId;
    const reply = new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`Timeout ${op}: ${stderr.slice(-1800)}`)), 10000);
      pending.set(id, value => { clearTimeout(timer); pending.delete(id); resolve(value); });
    });
    child.stdin.write(JSON.stringify({ id, op, params, ...(hello ? { engineId: hello.engineId, sessionId: hello.sessionId } : {}) }) + '\n');
    const value = await reply;
    if (errorCode) assert.equal(value.error?.code, errorCode, JSON.stringify(value));
    else assert.notEqual(value.kind, 'error', JSON.stringify(value));
    return value.data || value;
  }
  async function until(predicate) {
    let state;
    for (let n = 0; n < 150; n++) { state = await command('state.snapshot'); if (predicate(state)) return state; await delay(25); }
    throw new Error(`State condition timed out: ${JSON.stringify(state?.decks)}`);
  }
  async function record() {
    await delay(1100);
    await command('recording.start');
    const state = await until(s => s.recording.active);
    await delay(1400);
    await command('recording.stop');
    let wav;
    for (let n = 0; n < 80; n++) {
      await delay(50); wav = await readFile(state.recording.path);
      // EngineRecord closes the encoder asynchronously on its worker thread.
      if (wav.length > 1000 && wav.readUInt32LE(4) >= wav.length - 8) return wav;
    }
    return wav;
  }
  try {
    hello = await command('session.hello');
    await until(state => state.audio.applied);
    const markers = [125, 625, 1125, 1675, 2225, 2825, 3425, 4025, 4675, 5325, 6000, 6675];
    const cues = [0, 1250, null, null, null, null, null, 5000];
    for (const deck of ['A', 'B', 'C', 'D']) {
      await command('deck.load', { deck, track: { trackId: deck, path: audioPath, hotCues: cues, beatTimesMs: markers } });
      const loaded = await until(s => s.decks[deck].track);
      assert.deepEqual(loaded.decks[deck].hotCues, cues, `${deck} exact restored cues`);
      await command('deck.quantize.set', { deck, enabled: false });
      if (deck === 'A') {
        await command('deck.seek', { deck, positionMs: 1360 });
        await until(s => Math.abs(s.decks.A.positionMs - 1360) < 20);
        await command('deck.quantize.set', { deck, enabled: true });
        const exact = await command('deck.hotcue.set', { deck, index: 4, quantize: false });
        assert(Math.abs(exact.hotCues[4] - 1360) < 20); assert.equal(exact.quantize, true, 'per-command override preserves quantize setting');
        const quantized = await command('deck.hotcue.set', { deck, index: 5 });
        assert(Math.abs(quantized.hotCues[5] - 1125) < 30, 'registration uses native quantized grid');
        // クオンタイズは置き場所だけを拍へ寄せる。再生位置は据え置き。
        await delay(400);
        assert(Math.abs((await command('state.snapshot')).decks.A.positionMs - 1360) < 30,
          'quantized cue registration leaves the play position alone');
        await command('deck.quantize.set', { deck, enabled: true });
        await delay(400);
        assert(Math.abs((await command('state.snapshot')).decks.A.positionMs - 1360) < 30,
          'toggling quantize leaves the play position alone');
        await command('deck.quantize.set', { deck, enabled: false });
        await delay(400);
        assert(Math.abs((await command('state.snapshot')).decks.A.positionMs - 1360) < 30,
          'turning quantize off leaves the play position alone');
      }
      await command('deck.seek', { deck, positionMs: 1800 });
      await until(s => Math.abs(s.decks[deck].positionMs - 1800) < 30);
      const set = await command('deck.hotcue.set', { deck, trackId: deck, index: 2 });
      assert(Math.abs(set.hotCues[2] - 1800) < 30, `${deck} returned actual cue`);
      await command('deck.hotcue.set', { deck, index: 3, positionMs: 2300 });
      assert(Math.abs((await command('state.snapshot')).decks[deck].hotCues[3] - 2300) < 0.001);
      await command('deck.hotcue.jump', { deck, index: 7 });
      await until(s => Math.abs(s.decks[deck].positionMs - 5000) < 30);
      assert.notEqual((await command('state.snapshot')).decks[deck].status, 'playing', 'goto keeps paused deck paused');
      await command('deck.hotcue.clear', { deck, index: 2 });
      assert.equal((await command('state.snapshot')).decks[deck].hotCues[2], null);
      await command('deck.hotcue.set', { deck, trackId: 'stale', index: 0, positionMs: 100 }, 'invalid_params');
      await command('deck.seek', { deck, positionMs: 1125 });
      await until(s => Math.abs(s.decks[deck].positionMs - 1125) < 20);
      await command('deck.beatjump', { deck, beats: 4 });
      await until(s => Math.abs(s.decks[deck].positionMs - 3425) < 35);
      await command('deck.beatjump', { deck, beats: -4 });
      await until(s => Math.abs(s.decks[deck].positionMs - 1125) < 35);
      await command('deck.quantize.set', { deck, enabled: true });
      await command('deck.loop.beats', { deck, beats: 4 });
      const loop = (await until(s => s.decks[deck].loopRegion?.enabled)).decks[deck].loopRegion;
      assert(Math.abs(loop.startMs - 1125) < 35); assert(Math.abs(loop.endMs - 3425) < 35, `native variable-grid loop: ${JSON.stringify(loop)}`);
      await command('deck.loop.enable', { deck, enabled: false });
      await command('deck.quantize.set', { deck, enabled: false });
      await command('deck.loop.set', { deck, startMs: 6000, endMs: 6500 });
      await command('deck.loop.enable', { deck, enabled: true });
      await command('deck.seek', { deck, positionMs: 6400 });
      await command('deck.play', { deck }); await delay(700);
      const moving = (await command('state.snapshot')).decks[deck];
      assert(moving.positionMs >= 6000 && moving.positionMs <= 6535, 'audio engine actually wraps manual loop');
      await command('deck.pause', { deck });
      await command('deck.loop.enable', { deck, enabled: false });
      const eq = await command('mixer.channel.eq', { deck, band: 'low', gain: 0.5 });
      assert.equal(eq.channels[deck].eqLow, 0.5);
      await command('mixer.channel.eq', { deck, band: 'low', gain: 1 });
      const trim = await command('mixer.trim.set', { deck, gain: 0.5 }); assert.equal(trim.channels[deck].trim, 0.5);
      await command('mixer.trim.set', { deck, gain: 1 });
      // Every advertised pad effect must resolve to a Mixxx builtin manifest;
      // a name with no manifest fails the load and answers unsupported_operation.
      for (const effect of ['echo', 'reverb', 'flanger', 'phaser', 'filter', 'bitcrusher', 'distortion', 'autopan', 'tremolo', 'moogladder4filter']) {
        const state = await command('mixer.fx.set', { deck, effect, enabled: true, mix: 0.25 });
        assert.equal(state.channels[deck].fx.effect, effect);
        assert.equal(state.channels[deck].fx.enabled, true);
        // Insert effects replace the dry signal, so the host runs them fully wet.
        const insert = ['filter', 'bitcrusher', 'distortion', 'autopan', 'tremolo', 'moogladder4filter'].includes(effect);
        assert.equal(state.channels[deck].fx.mix, insert ? 1 : 0.25);
        await command('mixer.fx.set', { deck, effect, enabled: false, mix: 0.25 });
      }
    }
    for (const [op, params] of [
      ['deck.hotcue.set', { index: 16 }], ['deck.hotcue.set', { index: 0, positionMs: 24000 }],
      ['deck.loop.set', { startMs: 2000, endMs: 1000 }], ['deck.beatjump', { beats: 0 }],
      ['deck.loop.beats', { beats: 0.01 }], ['mixer.filter.set', { value: 2 }],
      ['mixer.trim.set', { gain: 3 }], ['mixer.channel.eq', { band: 'x', gain: 1 }],
      ['mixer.fx.set', { effect: 'echo', enabled: true, mix: 1.6 }],
      ['mixer.fx.set', { effect: 'nosucheffect', enabled: true, mix: 0.25 }],
      ['mixer.fx.set', { effect: 'echo', enabled: true, mix: 0.25, trackId: 'stale' }],
    ]) await command(op, { deck: 'A', ...params }, 'invalid_params');
    for (const hotCues of [[0], [0, 1, 2, 3, 4, 5, 6, -1], 'bad'])
      await command('deck.load', { deck: 'D', track: { trackId: 'invalid', path: audioPath, hotCues } }, 'invalid_params');
    await command('mixer.fx.set', { deck: 'D', effect: 'echo', enabled: true, mix: 0.5 });
    // デコード実尺を超えたキューはそのスロットだけ空にする。届かないキュー1個で
    // ロード全体を落とさない。ライブラリ側の保存値には触れていない。
    await command('deck.load', { deck: 'D', track: { trackId: 'past-end-cue', path: audioPath, hotCues: [25000, 1500, null, null, null, null, null, null] } });
    const dropped = await until(s => s.decks.D.track?.trackId === 'past-end-cue' && s.decks.D.status !== 'loading');
    assert.equal(dropped.decks.D.status, 'ready');
    assert.equal(dropped.decks.D.hotCues[0], null, 'an unreachable cue is dropped, not fatal');
    assert.equal(dropped.decks.D.track.hotCues[0], null, 'deck metadata reports what the engine actually holds');
    assert(Math.abs(dropped.decks.D.hotCues[1] - 1500) < 30, 'reachable cues survive');
    assert.equal(dropped.mixer.channels.D.fx.enabled, false, 'load resets FX');

    await command('deck.seek', { deck: 'A', positionMs: 0 });
    await command('deck.loop.set', { deck: 'A', startMs: 0, endMs: 16000 });
    await command('deck.loop.enable', { deck: 'A', enabled: true });
    await command('deck.play', { deck: 'A' });
    const flat = amplitudes(await record());
    assert(flat.every(value => value > 100), `audible baseline: ${flat}`);
    for (const [band, index] of [['low', 0], ['mid', 1], ['high', 2]]) {
      await command('mixer.channel.eq', { deck: 'A', band, gain: 0 });
      const cut = amplitudes(await record());
      assert(cut[index] < flat[index] * 0.18, `${band} actual PCM attenuation: ${cut} vs ${flat}`);
      await command('mixer.channel.eq', { deck: 'A', band, gain: 1 });
    }
    await command('mixer.filter.set', { deck: 'A', value: -0.7 });
    const lowpass = amplitudes(await record()); assert(lowpass[2] < flat[2] * 0.15, `LPF PCM: ${lowpass}`);
    await command('mixer.filter.set', { deck: 'A', value: 0.7 });
    const highpass = amplitudes(await record()); assert(highpass[0] < flat[0] * 0.15, `HPF PCM: ${highpass}`);
    await command('mixer.filter.set', { deck: 'A', value: 0 });
    await command('mixer.trim.set', { deck: 'A', gain: 0.5 });
    const trim = amplitudes(await record()); assert(trim.every((value, i) => value > flat[i] * 0.35 && value < flat[i] * 0.65), `trim PCM ${trim}`);
    await command('mixer.trim.set', { deck: 'A', gain: 1 });
    const fx = {};
    for (const effect of ['echo', 'reverb', 'flanger']) {
      await command('mixer.fx.set', { deck: 'A', effect, enabled: true, mix: 1, depth: 1 });
      const wet = amplitudes(await record());
      // The internal intensity and rack mix are both 0.5. A 3% spectral
      // difference is well above the <1% variation in these steady tones.
      if (effect === 'flanger') assert(wet.some((value, i) => Math.abs(value - flat[i]) > flat[i] * 0.03), `flanger changes PCM spectrum ${wet} vs ${flat}`);
      else assert(wet.minimumRms > Math.max(3, flat.minimumRms * 5), `${effect} produces a PCM tail: ${wet.minimumRms} vs ${flat.minimumRms}`);
      fx[effect] = { amplitudes: [...wet], minimumRms: wet.minimumRms };
      await command('mixer.fx.set', { deck: 'A', effect, enabled: false, mix: 1, depth: 1 });
    }
    console.log(JSON.stringify({ flat, lowpass, highpass, trim, fx }));
    for (const deck of ['A', 'B', 'C', 'D']) await command('mixer.fx.set', { deck, effect: 'echo', enabled: true, mix: 0.25 });
    const playingBeforeReconnect = (await command('state.snapshot')).decks.A.status;
    hello = await command('session.hello');
    const reconnected = await command('state.snapshot');
    for (const deck of ['A', 'B', 'C', 'D']) assert.equal(reconnected.mixer.channels[deck].fx.enabled, false, 'new session releases held FX');
    assert.equal(reconnected.decks.A.status, playingBeforeReconnect, 'reconnect does not stop deck playback');
  } finally {
    child.stdin.end();
    const result = await Promise.race([exited, delay(5000).then(() => null)]);
    if (!result) child.kill('SIGTERM');
    await rm(directory, { recursive: true, force: true });
    assert.equal(result?.code, 0, `host must exit cleanly: ${stderr.slice(-1600)}`);
  }
});
