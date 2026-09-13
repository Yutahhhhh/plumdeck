#!/usr/bin/env node
// Scripted remote performer for Junction Live screenshots and manual checks.
// Run it on the guest computer while the desktop app on the other computer is
// the session host (管理DJ). It joins with a pasted invite, prints the response
// to paste back, becomes the first DJ when the host starts the session, and
// keeps one current and one next track so the host shows a stable monitor pair.
//
//   node native/mixxx-engine-host/scripts/junction-tracks-fixture.mjs \
//     [--binary <engine>] [--track <audio file>]... [--invite <text> | --invite-file <file>] \
//     [--name "Remote DJ"] [--seconds 240] [--large-seconds 600] [--change-after 90] [--copy]
//
// Without --track it writes generated tones (--seconds long; a 240 s stereo tone
// is ~42 MB and takes over a minute at the paced Junction Live rate, so use
// e.g. --seconds 20 for a quick check). --large-seconds makes deck B a long
// WAV so the host stays in 「受信中」 long enough to capture it. --change-after
// loads another next track into deck B after N seconds. Stop with Ctrl+C.
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {mkdtemp, readFile, writeFile} from 'node:fs/promises';
import {rmSync} from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
const option = (name) => { const index = args.indexOf(name); return index >= 0 ? args[index + 1] : undefined; };
const options = (name) => args.flatMap((value, index) => value === name && args[index + 1] ? [args[index + 1]] : []);
const binary = option('--binary') || process.env.PLUMDECK_TEST_HOST
  || path.resolve(import.meta.dirname, process.platform === 'win32' ? '../stage/plumdeck-mixxx-engine-host.exe' : '../build-upstream/plumdeck-mixxx-engine-host');
const name = option('--name') || 'Remote DJ';
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const started = Date.now();
const log = (message) => console.log(`[fixture +${((Date.now() - started) / 1000).toFixed(1)}s] ${message}`);

function tone(seconds, frequency) {
  const rate = 44100, frames = rate * seconds, bytes = Buffer.alloc(44 + frames * 4);
  bytes.write('RIFF'); bytes.writeUInt32LE(bytes.length - 8, 4); bytes.write('WAVEfmt ', 8); bytes.writeUInt32LE(16, 16); bytes.writeUInt16LE(1, 20); bytes.writeUInt16LE(2, 22);
  bytes.writeUInt32LE(rate, 24); bytes.writeUInt32LE(rate * 4, 28); bytes.writeUInt16LE(4, 32); bytes.writeUInt16LE(16, 34); bytes.write('data', 36); bytes.writeUInt32LE(frames * 4, 40);
  for (let i = 0; i < frames; i++) {
    // A 128 BPM kick-like envelope gives the waveform visible structure.
    const beat = (i % Math.round(rate * 60 / 128)) / rate, envelope = Math.exp(-beat * 9) * 0.8 + 0.2;
    const value = Math.round(envelope * (7000 * Math.sin(i * 2 * Math.PI * frequency / rate) + 2500 * Math.sin(i * 2 * Math.PI * 55 / rate)));
    bytes.writeInt16LE(value, 44 + i * 4); bytes.writeInt16LE(value, 46 + i * 4);
  }
  bytes.writeUInt32LE(((Date.now() ^ frequency) >>> 0), 44); // unique content per run: a real transfer, not a cached hash
  return bytes;
}

function engine() {
  const child = spawn(binary, [], {
    stdio: ['pipe', 'pipe', 'inherit'],
    env: {...process.env, PLUMDECK_JUNCTION_EPHEMERAL_NETWORK: '1'},
  });
  let hello, id = 0; const pending = new Map();
  createInterface({input: child.stdout}).on('line', (line) => { let reply; try { reply = JSON.parse(line); } catch { return; } const entry = pending.get(reply.id); if (entry) { pending.delete(reply.id); entry(reply); } });
  child.on('exit', (code) => { console.error(`engine exited (${code})`); process.exit(code ?? 1); });
  const command = (op, params = {}) => new Promise((resolve, reject) => {
    const current = ++id; pending.set(current, (reply) => reply.kind === 'error' || reply.ok === false ? reject(new Error(`${op}: ${reply.error?.message}`)) : resolve(reply.data ?? reply));
    child.stdin.write(`${JSON.stringify({id: current, op, params, ...(hello ? {engineId: hello.engineId, sessionId: hello.sessionId} : {})})}\n`);
  });
  return {child, command, async start() { hello = await command('session.hello'); for (let i = 0; i < 200; i++) { if ((await command('state.snapshot')).audio.applied) return; await pause(100); } throw new Error('audio output did not start'); }};
}

async function until(read, predicate, label, timeout = 600000) {
  const end = Date.now() + timeout; let last;
  while (Date.now() < end) { last = await read(); if (predicate(last)) return last; await pause(250); }
  throw new Error(`${label}: timed out (${JSON.stringify(last?.connection)})`);
}

async function readInvite() {
  if (option('--invite')) return option('--invite');
  if (option('--invite-file')) return (await readFile(option('--invite-file'), 'utf8')).trim();
  console.log('ホストの「招待をコピー」で得た文字を貼り付けて Enter を押してください:');
  for await (const line of createInterface({input: process.stdin})) if (line.trim().startsWith('PLUMDECK-JUNCTION-')) return line.trim();
  throw new Error('invite text is required');
}

function copy(text) {
  const tool = process.platform === 'darwin' ? ['pbcopy', []] : process.platform === 'win32' ? ['clip', []] : null;
  if (!tool) return false;
  const child = spawn(tool[0], tool[1], {stdio: ['pipe', 'ignore', 'ignore']}); child.stdin.end(text); return true;
}

const directory = await mkdtemp(path.join(os.tmpdir(), 'plumdeck-junction-tracks-'));
const tracks = options('--track');
const files = [];
for (const [index, file] of tracks.entries()) files.push({path: path.resolve(file), title: path.parse(file).name, artist: name, id: `fixture-${index}`});
const seconds = Number(option('--seconds') || 240);
const generated = [[seconds, 440, 'Junction Live – Current'], [Number(option('--large-seconds') || seconds), 523, 'Junction Live – Next'], [Math.max(10, Math.round(seconds / 2)), 659, 'Junction Live – New Next']];
for (const [index, [seconds, frequency, title]] of generated.entries()) {
  if (files[index]) continue;
  const file = path.join(directory, `fixture-${index}.wav`); await writeFile(file, tone(seconds, frequency));
  files[index] = {path: file, title, artist: name, id: `fixture-${index}`};
}

log(`tracks: ${files.map((file) => path.basename(file.path)).join(', ')}`);
const peer = engine();
// Generated tones are large; remove them however the fixture ends.
process.on('exit', () => { try { rmSync(directory, {recursive: true, force: true}); } catch {} });
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => { peer.child.kill(); process.exit(0); });
await peer.start();
log('engine ready');
await peer.command('junction.network.configure', {stunUrls: [], turn: {}, save: false}).catch(() => undefined);
const load = async (deck, file, lease) => {
  await peer.command('deck.load', {deck, track: {trackId: file.id, path: file.path, title: file.title, artist: file.artist}, ...(lease ? {_junction: lease} : {})});
  await until(() => peer.command('state.snapshot'), (s) => ['ready', 'paused'].includes(s.decks[deck].status), `deck ${deck} decode`, 30000);
};
await load('A', files[0]); await load('B', files[1]); await peer.command('deck.play', {deck: 'A'});
log(`decks ready: A=${files[0].title} (playing), B=${files[1].title}`);

const invite = await readInvite();
await peer.command('junction.exchange.inspect', {text: invite});
await peer.command('junction.join', {displayName: name, djName: name, text: invite});
log('invite accepted; gathering the response');
const responded = await until(() => peer.command('junction.snapshot'), (s) => s.exchange?.responseText, 'response text', 60000);
console.log('\n--- この返答をホストの「返答を入力」に貼り付けて「このDJの接続を許可」を押してください ---\n');
console.log(responded.exchange.responseText);
if (args.includes('--copy') && copy(responded.exchange.responseText)) console.log('\n(返答をクリップボードへコピーしました)');
await until(() => peer.command('junction.snapshot'), (s) => s.connection?.state === 'connected', 'connection');
log('接続しました。ホストで「最初のDJに選ぶ」でこのDJを選んで開始してください。');
const live = await until(() => peer.command('junction.snapshot'), (s) => s.lifecycle === 'live' && s.performerPeerId === s.localPeerId, 'first performer');
log(`演奏担当になりました (epoch ${live.epoch})。ホストの Junction Live に現在再生中と次の曲が届きます。Ctrl+C で終了します。`);
const changeAfter = Number(option('--change-after') || 0);
if (changeAfter > 0) setTimeout(async () => {
  try {
    const s = await peer.command('junction.snapshot');
    await load('B', files[2], {sessionId: s.sessionId, epoch: s.epoch, actorPeerId: s.localPeerId});
    log(`deck B を ${files[2].title} に変更しました (次の曲カードの更新確認用)`);
  } catch (error) { console.error(String(error)); }
}, changeAfter * 1000);
for (;;) { await pause(10000); const s = await peer.command('junction.snapshot').catch(() => null); if (s) log(`${s.lifecycle} performer=${s.performerPeerId === s.localPeerId} connection=${s.connection?.state}`); }
