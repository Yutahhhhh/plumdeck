// Junction v2 loopback PCM bridge (junction::Link) against a real host.
// Opt-in: PLUMDECK_TEST_HOST=<built host>. Like the play suite it refuses to
// run unless BlackHole 2ch is the applied output; the program device is
// BlackHole too. Nothing reaches speakers.
//
// Checks: token-gated WebSocket upgrade, J input -> JUNCTION deck -> main mix
// -> "full" uplink, the "local" uplink excludes the J deck, and program PCM
// reaches the program device.
import test from 'node:test';
import assert from 'node:assert/strict';
import { rm } from 'node:fs/promises';
import { Host, delay, workspace } from './harness.mjs';

const binary = process.env.PLUMDECK_TEST_HOST;
const RATE = 48000;

function encode(kind, frameIndex, frames, render) {
  const buffer = new ArrayBuffer(16 + frames * 8);
  const view = new DataView(buffer);
  view.setUint8(0, kind); view.setUint8(1, 2); view.setUint32(4, frames, true); view.setFloat64(8, frameIndex, true);
  const pcm = new Float32Array(buffer, 16, frames * 2);
  for (let i = 0; i < frames; i += 1) { const v = render(frameIndex + i); pcm[2 * i] = v; pcm[2 * i + 1] = v; }
  return buffer;
}

/** Amplitude of `freq` in a mono signal (single-bin DFT). */
function tone(samples, freq) {
  let re = 0, im = 0;
  for (let i = 0; i < samples.length; i += 1) { re += samples[i] * Math.cos(2 * Math.PI * freq * i / RATE); im += samples[i] * Math.sin(2 * Math.PI * freq * i / RATE); }
  return 2 * Math.hypot(re, im) / samples.length;
}

test('Junction v2 PCM bridge on BlackHole 2ch', { timeout: 120000, skip: !binary && 'Set PLUMDECK_TEST_HOST to a built native host' }, async () => {
  const dir = await workspace();
  const h = await Host.start(binary, dir);
  try {
    const started = await h.ok('jlink.start');
    assert.equal(started.active, true);
    assert.ok(started.port > 0 && started.token.length >= 32);

    const refused = await new Promise((resolve) => {
      const bad = new WebSocket(`ws://127.0.0.1:${started.port}/jlink?token=wrong`);
      bad.onopen = () => { bad.close(); resolve(false); };
      bad.onerror = () => resolve(true);
    });
    assert.equal(refused, true, 'a wrong token must not upgrade');

    const ws = new WebSocket(`ws://127.0.0.1:${started.port}/jlink?token=${started.token}`);
    ws.binaryType = 'arraybuffer';
    await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('ws failed')); });
    let uplink = [];
    ws.onmessage = (event) => {
      const view = new DataView(event.data);
      if (view.getUint8(0) !== 3) return;
      const frames = view.getUint32(4, true);
      const pcm = new Float32Array(event.data, 16, frames * 2);
      for (let i = 0; i < frames; i += 1) uplink.push(0.5 * (pcm[2 * i] + pcm[2 * i + 1]));
    };

    await h.ok('jlink.input.set', { volume: 1, orientation: 1 });
    await h.ok('mixer.master.gain', { gain: 0.2 });
    await h.ok('jlink.uplink', { mode: 'full' });

    // Feed 440 Hz into the J deck in real time (1024-frame blocks).
    let frame = 0;
    const feeder = setInterval(() => {
      ws.send(encode(1, frame, 1024, (n) => 0.25 * Math.sin(2 * Math.PI * 440 * n / RATE)));
      frame += 1024;
    }, (1024 / RATE) * 1000);
    try {
      await delay(2500);
      uplink = [];
      await delay(1000);
      const full = Float32Array.from(uplink);
      assert.ok(full.length > RATE * 0.8, `uplink frames ${full.length}`);
      const fullTone = tone(full, 440);
      assert.ok(fullTone > 0.1, `J deck audio missing from the full uplink: ${fullTone}`);

      await h.ok('jlink.uplink', { mode: 'local' });
      await delay(500);
      uplink = [];
      await delay(1000);
      const local = Float32Array.from(uplink);
      assert.ok(local.length > RATE * 0.8, `local uplink frames ${local.length}`);
      assert.ok(tone(local, 440) < 0.002, `J deck leaked into the local uplink: ${tone(local, 440)}`);

      const state = await h.ok('jlink.state');
      assert.equal(state.input.receiving, true);
      assert.ok(state.input.latencyMs > 0 && state.input.latencyMs < 400, `J latency ${state.input.latencyMs}`);
    } finally {
      clearInterval(feeder);
    }

    // Program: open BlackHole as the program device and stream a tone to it.
    const devices = (await h.ok('audio.devices.list')).devices;
    const blackhole = devices.find((d) => d.name === 'BlackHole 2ch' && d.outputChannels >= 2);
    assert.ok(blackhole, 'BlackHole 2ch must exist');
    await h.ok('jlink.program.open', { deviceId: blackhole.id });
    let programFrame = 0;
    const programFeeder = setInterval(() => {
      ws.send(encode(2, programFrame, 1024, (n) => 0.2 * Math.sin(2 * Math.PI * 1000 * n / RATE)));
      programFrame += 1024;
    }, (1024 / RATE) * 1000);
    try {
      await delay(1500);
      const state = await h.ok('jlink.state');
      assert.equal(state.program.open, true);
      assert.ok(state.program.peak > 0.1, `program peak ${state.program.peak}`);
    } finally {
      clearInterval(programFeeder);
    }
    await h.ok('jlink.program.close');
    const stopped = await h.ok('jlink.stop');
    assert.equal(stopped.active, false);
    ws.close();
  } finally {
    await h.close();
    await rm(dir, { recursive: true, force: true });
  }
});
