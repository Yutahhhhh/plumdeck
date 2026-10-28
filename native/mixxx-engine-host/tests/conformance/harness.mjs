// Engine conformance harness: spawns a host over NDJSON, refuses to play
// unless BlackHole 2ch is the applied output, writes synthetic fixtures and
// returns master recordings as Float64Arrays. Every WAV is deleted after use.
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { mkdtemp, open, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';

export const RATE = 44100;
export const OUTPUT_DEVICE = 'BlackHole 2ch';
export const delay = ms => new Promise(resolve => setTimeout(resolve, ms));

// ------------------------------------------------------------------ fixtures

/** Deterministic PRNG so every run plays the same noise and clicks. */
function random(seed) {
  let state = seed >>> 0;
  return () => { state = (state * 1664525 + 1013904223) >>> 0; return state / 2 ** 32 * 2 - 1; };
}

/** A 1 ms broadband click; also the correlation template for echo timing. */
export function clickTemplate() {
  const next = random(7), length = Math.round(RATE * 0.001), out = new Float64Array(length);
  for (let i = 0; i < length; ++i) out[i] = 0.5 * next() * Math.sin(Math.PI * i / (length - 1));
  return out;
}

export const FIXTURES = {
  // T1/T2: per beat a 3 ms click, then 80 Hz / 1 kHz / 8 kHz bursts for 10..200 ms.
  pulse120: { seconds: 40, bpm: 120, render: pulse(120) },
  pulse128: { seconds: 40, bpm: 128, render: pulse(128) },
  // T3: white noise, -12 dBFS RMS, peak -7.3 dBFS.
  noise: { seconds: 20, bpm: 120, render: (() => { const next = random(3); return () => 0.433 * next(); })() },
  // T4/T6: 1 kHz sine at -12 dBFS (also the KEY SYNC tone).
  sine: { seconds: 40, bpm: 120, render: n => 0.25 * Math.sin(2 * Math.PI * 1000 * n / RATE) },
  // Meter calibration: 1 kHz sine at -6 dBFS.
  sine6: { seconds: 20, bpm: 120, render: n => 0.5 * Math.sin(2 * Math.PI * 1000 * n / RATE) },
  // T5: one click every 4 s (4 beats at 120 BPM leaves room for a 4-beat echo).
  clicks: { seconds: 40, bpm: 120, render: (() => { const t = clickTemplate(), period = RATE * 4; return n => (n % period < t.length ? t[n % period] : 0); })() },
  // Gate phase: continuous 1 kHz plus a click on every beat for the grid.
  grid: { seconds: 40, bpm: 120, render: (() => { const t = clickTemplate(), period = RATE / 2; return n => 0.2 * Math.sin(2 * Math.PI * 1000 * n / RATE) + (n % period < t.length ? t[n % period] : 0); })() },
};

function pulse(bpm) {
  const next = random(1), beat = 60 / bpm;
  return n => {
    const t = n / RATE, phase = t % beat;
    let v = 0;
    if (phase < 0.003) v += 0.4 * next() * Math.exp(-phase / 0.001);
    if (phase >= 0.010 && phase < 0.200) {
      const ramp = Math.min(1, Math.min(phase - 0.010, 0.200 - phase) / 0.002);
      v += 0.12 * ramp * (Math.sin(2 * Math.PI * 80 * t) + Math.sin(2 * Math.PI * 1000 * t) + Math.sin(2 * Math.PI * 8000 * t));
    }
    return v;
  };
}

/**
 * Writes a 16-bit stereo WAV whose header claims `totalSeconds`; only
 * `seconds` are rendered and the rest is a sparse zero extension, so long
 * loops (512 beats) fit without writing hundreds of megabytes.
 */
export async function writeFixture(file, name, totalSeconds = 300) {
  const { seconds, render } = FIXTURES[name];
  const frames = Math.round(RATE * seconds), total = Math.round(RATE * totalSeconds);
  const header = Buffer.alloc(44);
  header.write('RIFF'); header.writeUInt32LE(36 + total * 4, 4); header.write('WAVEfmt ', 8);
  header.writeUInt32LE(16, 16); header.writeUInt16LE(1, 20); header.writeUInt16LE(2, 22);
  header.writeUInt32LE(RATE, 24); header.writeUInt32LE(RATE * 4, 28); header.writeUInt16LE(4, 32); header.writeUInt16LE(16, 34);
  header.write('data', 36); header.writeUInt32LE(total * 4, 40);
  const body = Buffer.alloc(frames * 4);
  for (let n = 0; n < frames; ++n) {
    const value = Math.max(-32768, Math.min(32767, Math.round(render(n) * 32767)));
    body.writeInt16LE(value, n * 4); body.writeInt16LE(value, n * 4 + 2);
  }
  await writeFile(file, Buffer.concat([header, body]));
  const handle = await open(file, 'r+');
  try { await handle.truncate(44 + total * 4); } finally { await handle.close(); }
}

// ------------------------------------------------------------------ WAV

export function decodeWav(wav) {
  let data, rate, bits, channels, format;
  for (let at = 12; at + 8 <= wav.length;) {
    const type = wav.toString('ascii', at, at + 4), length = wav.readUInt32LE(at + 4);
    if (type === 'fmt ') { format = wav.readUInt16LE(at + 8); channels = wav.readUInt16LE(at + 10); rate = wav.readUInt32LE(at + 12); bits = wav.readUInt16LE(at + 22); }
    if (type === 'data') data = wav.subarray(at + 8, Math.min(wav.length, at + 8 + length));
    at += 8 + length + (length % 2);
  }
  if (!data || channels !== 2) throw new Error(`Unexpected recording format: ${channels} channels, ${bits} bits`);
  const width = bits / 8, frames = Math.floor(data.length / (width * 2));
  const left = new Float64Array(frames), right = new Float64Array(frames), mono = new Float64Array(frames);
  const read = bits === 16 ? o => data.readInt16LE(o) / 32768
    : bits === 24 ? o => data.readIntLE(o, 3) / 8388608
    : bits === 32 && format === 3 ? o => data.readFloatLE(o) : o => data.readInt32LE(o) / 2147483648;
  for (let n = 0; n < frames; ++n) {
    left[n] = read(n * width * 2); right[n] = read(n * width * 2 + width); mono[n] = (left[n] + right[n]) / 2;
  }
  return { rate, left, right, mono, frames };
}

// ------------------------------------------------------------------ host

export class Host {
  static async start(binary, workDir) {
    const host = new Host(binary, workDir);
    await host.#hello();
    return host;
  }

  constructor(binary, workDir) {
    this.workDir = workDir; this.events = []; this.stderr = ''; this.pending = new Map(); this.id = 0; this.meters = [];
    this.child = spawn(binary, ['--tick-ms=20'], {
      cwd: workDir,
      env: { ...process.env, PLUMDECK_MIXXX_OUTPUT_DEVICE: OUTPUT_DEVICE, PLUMDECK_MIXXX_RECORDING_DIR: workDir,
        PLUMDECK_WAVEFORM_CACHE: path.join(workDir, 'waveforms'), TMPDIR: workDir + '/' },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    this.exited = once(this.child, 'exit');
    this.child.stderr.on('data', bytes => { this.stderr = (this.stderr + bytes).slice(-20000); });
    this.child.on('error', error => { for (const entry of this.pending.values()) entry.reject(error); });
    createInterface({ input: this.child.stdout }).on('line', line => {
      let message;
      try { message = JSON.parse(line); } catch { return; }
      if (message.kind === 'event') {
        if (message.event === 'meters') { this.meters.push({ at: performance.now(), data: message.data }); if (this.meters.length > 2000) this.meters.shift(); }
        else if (message.event !== 'deck.position' && message.event !== 'deck.clock.v2') { this.events.push({ at: performance.now(), ...message }); if (this.events.length > 5000) this.events.shift(); }
        return;
      }
      const entry = this.pending.get(message.id);
      if (!entry) return;
      clearTimeout(entry.timer); this.pending.delete(message.id); entry.resolve(message);
    });
  }

  async #hello() {
    this.hello = await this.raw('session.hello');
    if (this.hello.kind === 'error') throw new Error(`session.hello failed: ${JSON.stringify(this.hello)}`);
    this.engine = this.hello.engine;
    const start = Date.now();
    let state;
    while (Date.now() - start < 20000) {
      state = await this.snapshot();
      if (state.audio?.applied || state.audio?.reason) break;
      await delay(50);
    }
    // Fail closed: never play anything unless the applied device is BlackHole.
    if (state?.audio?.deviceId !== OUTPUT_DEVICE || !state?.audio?.applied) {
      await this.close();
      throw new Error(`Refusing to play: applied output is ${JSON.stringify(state?.audio?.deviceId)} (applied=${state?.audio?.applied})`);
    }
    this.audio = state.audio; this.engine = state.engine;
  }

  /** Sends a command and resolves with the raw reply (result or error). */
  raw(op, params = {}, timeoutMs = 15000) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`${op} timed out; stderr: ${this.stderr.slice(-1500)}`)); }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      const envelope = { id, op, params, ...(this.hello?.sessionId ? { sessionId: this.hello.sessionId, engineId: this.hello.engineId } : {}) };
      this.child.stdin.write(JSON.stringify(envelope) + '\n');
    });
  }

  /** Command that must succeed; returns its data. */
  async ok(op, params = {}) {
    const reply = await this.raw(op, params);
    if (reply.kind === 'error') throw new CommandError(op, reply.error);
    return reply.data ?? reply;
  }

  /** Command whose rejection is a measured outcome: returns { ok, data, error }. */
  async try(op, params = {}) {
    const reply = await this.raw(op, params);
    return reply.kind === 'error' ? { ok: false, error: reply.error } : { ok: true, data: reply.data ?? reply };
  }

  snapshot() { return this.ok('state.snapshot'); }

  async until(predicate, what = 'state', timeoutMs = 6000) {
    const start = Date.now();
    let state;
    while (Date.now() - start < timeoutMs) {
      state = await this.snapshot();
      if (predicate(state)) return state;
      await delay(10);
    }
    throw new Error(`Timed out waiting for ${what}`);
  }

  has(capability) { return (this.engine?.capabilities ?? []).includes(capability); }

  async load(deck, file, { bpm = 120, musicalKey, trackId = `${deck}-${path.basename(file)}` } = {}) {
    await this.ok('deck.load', { deck, track: { trackId, path: file, bpm, beatgridOffsetMs: 0, beatsPerBar: 4, ...(musicalKey ? { musicalKey } : {}) } });
    return this.until(s => s.decks[deck].status === 'ready' && s.decks[deck].track?.trackId === trackId, `deck ${deck} load`, 20000);
  }

  /**
   * Records the master bus while `during(mark)` runs, then for `tailMs`.
   * `mark(name)` stores wall-clock offsets from the recording start, so
   * analysis can find command times in the WAV to within a few buffers.
   */
  async record(during, tailMs = 1000) {
    await delay(1200); // The host refuses a new take within ~1.1 s of the last.
    // The WAV starts when the host accepts recording.start, not when the
    // snapshot first reports it active; time marks from the request.
    const zero = performance.now(), marks = {};
    const started = await this.ok('recording.start');
    const state = await this.until(s => s.recording?.active, 'recording start');
    const mark = name => { marks[name] = (performance.now() - zero) / 1000; };
    if (during) await during(mark);
    await delay(tailMs);
    await this.ok('recording.stop');
    await this.until(s => !s.recording?.active && !s.recording?.stopping, 'recording finalization', 15000);
    const file = state.recording.path ?? started.path;
    let audio;
    for (let attempt = 0; attempt < 60; ++attempt) {
      try { audio = decodeWav(await readFile(file)); if (audio.frames > 0) break; } catch { /* finalizing */ }
      await delay(50);
    }
    await rm(file, { force: true });
    if (!audio) throw new Error(`Recording ${file} could not be read`);
    return { ...audio, marks };
  }

  async close() {
    for (const entry of this.pending.values()) { clearTimeout(entry.timer); entry.reject(new Error('host closing')); }
    this.pending.clear();
    if (this.child.exitCode === null) {
      this.child.stdin.end();
      const timer = setTimeout(() => this.child.kill('SIGTERM'), 4000);
      const hard = setTimeout(() => this.child.kill('SIGKILL'), 9000);
      await this.exited; clearTimeout(timer); clearTimeout(hard);
    }
  }
}

export class CommandError extends Error {
  constructor(op, error) { super(`${op}: ${error?.code} ${error?.message}`); this.code = error?.code; }
}

export async function workspace() {
  return mkdtemp(path.join(process.env.PLUMDECK_CONFORMANCE_TMP || tmpdir(), 'plumdeck-conformance-'));
}
