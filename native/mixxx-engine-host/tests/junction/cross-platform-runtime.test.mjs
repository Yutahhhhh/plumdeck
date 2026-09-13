// Cross-machine manual Junction admission. The local process and a Windows
// process reached through SSH exchange exactly the same text as the desktop UI.
// Run explicitly; normal CI has no second machine:
//   PLUMDECK_JUNCTION_CROSS_REMOTE=user@host \
//   PLUMDECK_JUNCTION_CROSS_REMOTE_BINARY='C:\\path\\to\\plumdeck-mixxx-engine-host.exe' \
//   node --test native/mixxx-engine-host/tests/junction/cross-platform-runtime.test.mjs
// To additionally exercise a real remote-first performance, Program audio,
// track transfer, and waveform access in both directions, copy a WAV to the
// Windows computer and also set:
//   PLUMDECK_JUNCTION_CROSS_REMOTE_TRACK='C:\\path\\to\\tone.wav'
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdtemp, rm, writeFile} from 'node:fs/promises';
import os from 'node:os';
import test from 'node:test';
import path from 'node:path';

const localBinary = process.env.PLUMDECK_TEST_HOST
  || path.resolve(import.meta.dirname, '../../build-upstream/plumdeck-mixxx-engine-host');
const remote = process.env.PLUMDECK_JUNCTION_CROSS_REMOTE;
const remoteBinary = process.env.PLUMDECK_JUNCTION_CROSS_REMOTE_BINARY;
const remoteTrack = process.env.PLUMDECK_JUNCTION_CROSS_REMOTE_TRACK;
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(read, predicate, label, timeout = 50000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    last = await read();
    if (predicate(last)) return last;
    await pause(80);
  }
  throw new Error(`${label}: timed out; state=${JSON.stringify({
    connection:last?.connection,
    exchange:last?.exchange && {state:last.exchange.state, detail:last.exchange.detail, errorCode:last.exchange.errorCode},
  })}`);
}

function wrap(child, label) {
  let hello;
  let id = 0;
  let stderr = '';
  const pending = new Map();
  child.stderr.on('data', (data) => {
    stderr = (stderr + data).slice(-16000);
    if (process.env.PLUMDECK_JUNCTION_TRACE) process.stderr.write(`[${label}] ${data}`);
  });
  const rejectAll = (reason) => {
    for (const entry of pending.values()) {
      clearTimeout(entry.timer);
      entry.reject(reason);
    }
    pending.clear();
  };
  child.on('error', rejectAll);
  child.on('exit', (code) => rejectAll(new Error(`${label} exited (${code}): ${stderr}`)));
  createInterface({input:child.stdout}).on('line', (line) => {
    let reply;
    try { reply = JSON.parse(line); } catch { return; }
    const entry = pending.get(reply.id);
    if (!entry) return;
    pending.delete(reply.id);
    clearTimeout(entry.timer);
    entry.resolve(reply);
  });
  const raw = (op, params = {}) => {
    const current = ++id;
    const result = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(current);
        reject(new Error(`${label} ${op} timed out: ${stderr}`));
      }, 20000);
      pending.set(current, {resolve, reject, timer});
    });
    child.stdin.write(`${JSON.stringify({id:current, op, params, ...(hello ? {engineId:hello.engineId, sessionId:hello.sessionId} : {})})}\n`);
    return result;
  };
  const command = async (op, params = {}) => {
    const reply = await raw(op, params);
    assert.notEqual(reply.kind, 'error', `${label} ${op}: ${reply.error?.message || 'native error'}`);
    assert.notEqual(reply.ok, false, `${label} ${op}: ${reply.error?.message || 'native error'}`);
    return reply.data ?? reply;
  };
  return {
    label,
    command,
    snapshot: () => command('junction.snapshot'),
    diagnostics: () => stderr.replace(/PLUMDECK-JUNCTION-\S+/g, '[exchange redacted]'),
    async start() {
      hello = await command('session.hello');
      assert.equal(hello.engine.implementation, 'mixxx');
      await command('junction.network.configure', {stunUrls:[], turn:{}, save:false});
    },
    async close() {
      if (child.exitCode !== null) return;
      child.stdin.end();
      await Promise.race([new Promise((resolve) => child.once('exit', resolve)), pause(5000)]);
      if (child.exitCode === null) child.kill('SIGKILL');
    },
  };
}

function localPeer(label) {
  return wrap(spawn(localBinary, [], {env:{...process.env, PLUMDECK_JUNCTION_EPHEMERAL_NETWORK:'1'}}), label);
}

function windowsPeer(label) {
  const escaped = remoteBinary.replaceAll("'", "''");
  const command = `$env:PLUMDECK_JUNCTION_EPHEMERAL_NETWORK='1'; $env:PLUMDECK_JUNCTION_TRACE='1'; Set-Location (Split-Path '${escaped}'); & '${escaped}'`;
  return wrap(spawn('ssh', ['-T', '-o', 'BatchMode=yes', remote, command]), label);
}

const participant = (snapshot, peerId) => snapshot.participants.find((entry) => entry.peerId === peerId);

function tone(seconds = 45) {
  const rate = 44100, frames = rate * seconds, bytes = Buffer.alloc(44 + frames * 4);
  bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8); bytes.writeUInt32LE(16, 16);
  bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(2, 22); bytes.writeUInt32LE(rate, 24); bytes.writeUInt32LE(rate * 4, 28);
  bytes.writeUInt16LE(4, 32); bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(frames * 4, 40);
  for (let frame = 0; frame < frames; frame++) {
    const beat = (frame % Math.round(rate * 60 / 128)) / rate;
    const envelope = Math.exp(-beat * 9) * .8 + .2;
    const value = Math.round(envelope * (7000 * Math.sin(frame * 2 * Math.PI * 440 / rate) + 2500 * Math.sin(frame * 2 * Math.PI * 55 / rate)));
    bytes.writeInt16LE(value, 44 + frame * 4); bytes.writeInt16LE(value, 46 + frame * 4);
  }
  bytes.writeUInt32LE(Date.now() >>> 0, 44);
  return bytes;
}

async function programDevice(peer, platform) {
  const configured = process.env[platform === 'windows' ? 'PLUMDECK_JUNCTION_CROSS_WINDOWS_PROGRAM_DEVICE' : 'PLUMDECK_JUNCTION_CROSS_MAC_PROGRAM_DEVICE'];
  if (configured) return configured;
  const listed = await peer.command('audio.devices.list');
  const outputs = listed.devices.filter((device) => device.outputChannels >= 2);
  const selected = outputs.find((device) => platform === 'mac' && device.name.includes('BlackHole'))
    ?? outputs.find((device) => device.isDefault)
    ?? outputs[0];
  assert(selected, `${platform} has no stereo Program output`);
  return selected.id.replace(/^[^:]+:/, '');
}

async function connect(host, guest) {
  await Promise.all([host.start(), guest.start()]);
  await host.command('junction.create', {
    djName:`${host.label} DJ`, sessionName:`${host.label} host`, adoptCurrent:false,
    startInLobby:true, exchangeMode:'manual',
  });
  const before = await host.snapshot();
  await host.command('junction.invite.create');
  const invited = await until(host.snapshot, (snapshot) => snapshot.participants.some((entry) =>
    entry.peerId !== snapshot.localPeerId && entry.exchange?.inviteText), `${host.label} invitation`);
  const slot = invited.participants.find((entry) => entry.peerId !== invited.localPeerId && entry.exchange?.inviteText);
  assert(slot);
  await guest.command('junction.exchange.inspect', {text:slot.exchange.inviteText});
  await guest.command('junction.join', {djName:`${guest.label} DJ`, text:slot.exchange.inviteText});
  const answered = await until(guest.snapshot, (snapshot) => snapshot.exchange?.responseText, `${guest.label} response`);
  assert.equal(answered.exchange.state, 'response_ready');
  await host.command('junction.exchange.import', {peerId:slot.peerId, text:answered.exchange.responseText});
  const pending = await host.snapshot();
  assert.equal(participant(pending, slot.peerId)?.exchange?.state, 'approval_pending');
  await pause(1000);
  assert.equal((await guest.snapshot()).exchange.state, 'response_ready', 'import alone must not tear down the guest');
  await host.command('junction.peer.approve', {peerId:slot.peerId, accept:true});
  await Promise.all([
    until(host.snapshot, (snapshot) => participant(snapshot, slot.peerId)?.exchange?.state === 'connected', `${host.label} connected`),
    until(guest.snapshot, (snapshot) => snapshot.exchange?.state === 'connected' && snapshot.connection?.state === 'connected', `${guest.label} connected`),
  ]);
  await host.command('junction.end');
  await until(guest.snapshot, (snapshot) => !snapshot.active, `${guest.label} ended`);
  assert(before.active);
}

async function connectLive(host, guest, guestTrack, hostPlatform) {
  await Promise.all([host.start(), guest.start()]);
  await guest.command('deck.load', {deck:'A', track:{trackId:`cross-${guest.label}`, path:guestTrack, title:'Cross-platform Junction Live', artist:guest.label}});
  await until(() => guest.command('state.snapshot'), (snapshot) => ['ready', 'paused'].includes(snapshot.decks.A.status), `${guest.label} track decode`, 30000);
  await guest.command('deck.play', {deck:'A'});
  await host.command('junction.create', {
    djName:`${host.label} DJ`, sessionName:`${host.label} live`, programDevice:await programDevice(host, hostPlatform),
    adoptCurrent:false, startInLobby:true, exchangeMode:'manual',
  });
  await host.command('junction.invite.create');
  const invited = await until(host.snapshot, (snapshot) => snapshot.participants.some((entry) =>
    entry.peerId !== snapshot.localPeerId && entry.exchange?.inviteText), `${host.label} invitation`);
  const slot = invited.participants.find((entry) => entry.peerId !== invited.localPeerId && entry.exchange?.inviteText);
  assert(slot);
  await guest.command('junction.exchange.inspect', {text:slot.exchange.inviteText});
  await guest.command('junction.join', {djName:`${guest.label} DJ`, text:slot.exchange.inviteText});
  const answered = await until(guest.snapshot, (snapshot) => snapshot.exchange?.responseText, `${guest.label} response`);
  await host.command('junction.exchange.import', {peerId:slot.peerId, text:answered.exchange.responseText});
  await host.command('junction.peer.approve', {peerId:slot.peerId, accept:true});
  await Promise.all([
    until(host.snapshot, (snapshot) => participant(snapshot, slot.peerId)?.exchange?.state === 'connected', `${host.label} connected`),
    until(guest.snapshot, (snapshot) => snapshot.exchange?.state === 'connected', `${guest.label} connected`),
  ]);
  await host.command('junction.session.start', {performerPeerId:slot.peerId});
  await Promise.all([
    until(host.snapshot, (snapshot) => snapshot.lifecycle === 'live' && snapshot.performerPeerId === slot.peerId, `${host.label} remote-first live`, 90000),
    until(guest.snapshot, (snapshot) => snapshot.lifecycle === 'live' && snapshot.performerPeerId === snapshot.localPeerId, `${guest.label} performer live`, 90000),
  ]);
  const monitored = await until(host.snapshot, (snapshot) => snapshot.junctionTracks?.some((track) =>
    track.role === 'current' && track.title === 'Cross-platform Junction Live' && track.state === 'ready'), `${host.label} Junction Live asset`, 120000);
  const current = monitored.junctionTracks.find((track) => track.role === 'current');
  assert.match(current.assetId, /^[0-9a-f]{64}$/);
  assert.equal(current.playing, true);
  assert.notEqual(current.path, guestTrack, 'the coordinator uses its own verified cache path');
  const waveform = await host.command('waveform.ensure', {junctionAssetId:current.assetId});
  assert(waveform.assetKey, 'the coordinator can build a waveform without deck.load');
  const nativeState = await host.command('state.snapshot');
  assert.equal(nativeState.decks.A.track, null, 'Junction Live never replaces the coordinator deck');
  await until(host.snapshot, (snapshot) => Number(snapshot.program?.rms) > .001, `${host.label} Program audio`, 15000);
  const initialPosition = current.positionMs;
  await until(host.snapshot, (snapshot) => snapshot.junctionTracks?.find((track) => track.role === 'current')?.positionMs > initialPosition + 200, `${host.label} live position`);
  await host.command('junction.end');
  await until(guest.snapshot, (snapshot) => !snapshot.active, `${guest.label} ended`);
}

test('manual Junction connects with macOS host and Windows guest, then reversed', {
  timeout:180000,
  skip:!remote || !remoteBinary ? 'set cross-platform SSH environment variables' : false,
}, async () => {
  for (const direction of ['mac-host', 'windows-host']) {
    const mac = localPeer(`mac-${direction}`);
    const windows = windowsPeer(`windows-${direction}`);
    try {
      await connect(direction === 'mac-host' ? mac : windows, direction === 'mac-host' ? windows : mac);
    } catch (error) {
      error.message += `\nmac diagnostics:\n${mac.diagnostics()}\nwindows diagnostics:\n${windows.diagnostics()}`;
      throw error;
    } finally {
      await Promise.allSettled([mac.close(), windows.close()]);
    }
  }
});

test('remote-first Junction Live transfers audio, exposes a waveform, and keeps Program audio live in both OS directions', {
  timeout:360000,
  skip:!remote || !remoteBinary || !remoteTrack ? 'also set the Windows WAV path' : false,
}, async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'plumdeck-cross-live-'));
  const localTrack = path.join(directory, 'cross-live.wav');
  await writeFile(localTrack, tone());
  try {
    for (const direction of ['mac-host', 'windows-host']) {
      const mac = localPeer(`mac-${direction}`);
      const windows = windowsPeer(`windows-${direction}`);
      try {
        await connectLive(direction === 'mac-host' ? mac : windows, direction === 'mac-host' ? windows : mac,
          direction === 'mac-host' ? remoteTrack : localTrack, direction === 'mac-host' ? 'mac' : 'windows');
      } catch (error) {
        error.message += `\nmac diagnostics:\n${mac.diagnostics()}\nwindows diagnostics:\n${windows.diagnostics()}`;
        throw error;
      } finally {
        await Promise.allSettled([mac.close(), windows.close()]);
      }
    }
  } finally {
    await rm(directory, {recursive:true, force:true});
  }
});
