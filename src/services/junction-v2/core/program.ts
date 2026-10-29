// The host's program: one continuous stream spliced from the DJs' uplinks.
//
// Clock domain. Every stream the host receives (each guest's uplink, and the
// host's own engine mix) is rendered by one AudioContext and captured by one
// AudioWorklet, so all of them share a single sample clock: "capture frame" f
// means the same instant for every stream. Network jitter, codec delay and
// clock drift between the DJs' machines are absorbed upstream by WebRTC's
// jitter buffers and the elastic buffers; this module only ever sees aligned,
// gap-free capture frames. `ingest()` appends them; `now` is the end of what
// has been captured.
//
// Fixed delay. The program reads the on-air stream at position `pos` and
// runs `delay = now − pos` behind capture, nominally D (a few seconds; the
// stream is a broadcast, so latency is free and buys room for every splice).
//
// Handoff (A on air -> B). B's uplink already contains A as B heard it on the
// J deck, delayed by the full relay loop L (host -> B -> host):
//      B[f] ≈ g·A[f − L] + B's own decks
// To keep A's audio continuous, the program must jump from A at source frame s
// to B at frame s + L — then the A content in B continues exactly where A left
// off: no repeat (echo), no skip (jump). Steps:
//   1. Measure L on the audio itself (align.ts: GCC-PHAT coarse + normalized
//      correlation fine, sample accurate). The prior from timestamps (RTT and
//      the guest's reported buffer latencies) only narrows the search.
//   2. Choose s = pos + guard (just ahead of the reader, so the change is heard
//      as soon as possible), and require s + L + fade ≤ now, i.e. L must fit in
//      the delay we hold. With D = 3 s and typical loops of 0.2–0.8 s it does.
//   3. Crossfade over `fade` frames from A[s..] to B[s+L..]. Both carry the
//      same A content sample-aligned, so the crossfade is linear (coherent
//      sum stays at unity) and inaudible for the A content; B's own decks
//      fade in over the same frames.
//   4. Continue reading B at pos + L. The program is now D − L behind capture.
// Delay recovery. Two seconds after a handoff (the splice itself plays
// sample-exact at rate 1), each handoff spends L of the delay. The reader then plays
// at a rate slightly below 1 (at most `maxRateDeviation`, 0.2 % ≈ 3.5 cents,
// slewed in over seconds) until the delay is back to D, then locks at exactly
// 1. The same loop absorbs any residual drift. Recovering 0.5 s takes ~4 min,
// shorter than a typical turn; if the next NEXT comes earlier, the remaining
// budget is simply smaller.
// Fallbacks (reported, never silent):
//   unaligned  the correlation found no A content in B (B had the J deck
//              faded out). Nothing can echo, so splice at the prior lag.
//   late       L does not fit in the current delay. Splice at the largest lag
//              that fits; the A content repeats by the difference.
import { estimateLag } from "./align.ts";

export interface ProgramOptions {
  sampleRate?: number;
  delaySeconds?: number;
  historySeconds?: number;
  fadeMs?: number;
  guardMs?: number;
  maxRateDeviation?: number;
  deadbandMs?: number;
  windowSeconds?: number;
  maxLagSeconds?: number;
  minConfidence?: number;
}

export interface StereoBlock {
  left: Float32Array;
  right: Float32Array;
}

export type SpliceStatus = "started" | "aligned" | "unaligned" | "late" | "same";

export interface SplicePlan {
  status: SpliceStatus;
  from: string | null;
  to: string;
  /** Frames the next DJ's mix lags the on-air stream (what the splice skips). */
  lagFrames: number;
  /** Normalized correlation of the measurement (0 when not measured). */
  confidence: number;
  /** Source frame where the crossfade starts. */
  at: number;
}

/** Ring of stereo frames addressed by absolute capture frame. */
export class StreamHistory {
  readonly capacity: number;
  private readonly l: Float32Array;
  private readonly r: Float32Array;
  /** Absolute frame one past the newest written frame. */
  end = 0;

  constructor(capacity: number, start = 0) {
    this.capacity = capacity;
    this.l = new Float32Array(capacity);
    this.r = new Float32Array(capacity);
    this.end = start;
  }

  /** Writes frames at absolute position `frame0`. Gaps are zero-filled. */
  write(frame0: number, frames: number, left: Float32Array | null, right: Float32Array | null): void {
    if (frame0 > this.end) this.zero(this.end, frame0 - this.end);
    for (let i = 0; i < frames; i += 1) {
      const slot = (frame0 + i) % this.capacity;
      this.l[slot] = left ? left[i] : 0;
      this.r[slot] = right ? right[i] ?? left![i] : left ? left[i] : 0;
    }
    this.end = Math.max(this.end, frame0 + frames);
  }

  private zero(from: number, count: number) {
    const n = Math.min(count, this.capacity);
    for (let i = from + count - n; i < from + count; i += 1) {
      const slot = i % this.capacity;
      this.l[slot] = 0;
      this.r[slot] = 0;
    }
  }

  has(frame: number): boolean {
    return frame >= 0 && frame < this.end && frame >= this.end - this.capacity;
  }

  left(frame: number): number { return this.has(frame) ? this.l[frame % this.capacity] : 0; }
  right(frame: number): number { return this.has(frame) ? this.r[frame % this.capacity] : 0; }

  /** 4-point cubic Hermite read at a fractional frame. Exact at integers. */
  sample(pos: number, out: [number, number]): void {
    const i = Math.floor(pos);
    const t = pos - i;
    if (t === 0) { out[0] = this.left(i); out[1] = this.right(i); return; }
    out[0] = hermite(this.left(i - 1), this.left(i), this.left(i + 1), this.left(i + 2), t);
    out[1] = hermite(this.right(i - 1), this.right(i), this.right(i + 1), this.right(i + 2), t);
  }

  mono(start: number, length: number): Float32Array {
    const out = new Float32Array(length);
    for (let i = 0; i < length; i += 1) out[i] = 0.5 * (this.left(start + i) + this.right(start + i));
    return out;
  }
}

export function hermite(y0: number, y1: number, y2: number, y3: number, t: number): number {
  const c1 = 0.5 * (y2 - y0);
  const c2 = y0 - 2.5 * y1 + 2 * y2 - 0.5 * y3;
  const c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
  return ((c3 * t + c2) * t + c1) * t + y1;
}

interface PendingSplice {
  to: string;
  at: number;
  lag: number;
  done: number;
}

export class ProgramAssembler {
  readonly sampleRate: number;
  readonly delayFrames: number;
  private readonly historyFrames: number;
  private readonly fadeFrames: number;
  private readonly guardFrames: number;
  private readonly maxRateDeviation: number;
  private readonly deadbandFrames: number;
  private readonly windowFrames: number;
  private readonly maxLagFrames: number;
  private readonly minConfidence: number;
  private readonly histories = new Map<string, StreamHistory>();
  private started = false;
  /** Absolute capture frame one past the newest ingested frame. */
  now = 0;
  source: string | null = null;
  pos = 0;
  rate = 1;
  private locked = true;
  private pending: PendingSplice | null = null;
  private fadeIn = 0;
  private fadeOut = 0;
  /** Frames to keep the rate at exactly 1 after a splice (see control()). */
  private hold = 0;
  private readonly scratch: [number, number] = [0, 0];
  private readonly scratchB: [number, number] = [0, 0];

  constructor(options: ProgramOptions = {}) {
    this.sampleRate = options.sampleRate ?? 48000;
    const ms = (v: number) => Math.round((v * this.sampleRate) / 1000);
    this.delayFrames = Math.round((options.delaySeconds ?? 3) * this.sampleRate);
    this.historyFrames = Math.round((options.historySeconds ?? 12) * this.sampleRate);
    this.fadeFrames = ms(options.fadeMs ?? 30);
    this.guardFrames = ms(options.guardMs ?? 100);
    this.maxRateDeviation = options.maxRateDeviation ?? 0.002;
    this.deadbandFrames = ms(options.deadbandMs ?? 10);
    this.windowFrames = Math.round((options.windowSeconds ?? 4) * this.sampleRate);
    this.maxLagFrames = Math.round((options.maxLagSeconds ?? 2) * this.sampleRate);
    this.minConfidence = options.minConfidence ?? 0.2;
    if (this.historyFrames < this.delayFrames + this.windowFrames) {
      throw new Error("history must cover the delay plus the correlation window");
    }
  }

  /** Current program delay behind capture, in frames. */
  get delay(): number { return this.now - this.pos; }
  get splicing(): boolean { return this.pending !== null; }

  history(id: string): StreamHistory | undefined { return this.histories.get(id); }

  /**
   * Appends `frames` capture frames starting at `frame0` for every stream.
   * Streams not in `blocks` (disconnected, silent) are written as silence so
   * every history stays on the same clock.
   */
  ingest(frame0: number, frames: number, blocks: Record<string, StereoBlock | undefined>): void {
    if (!this.started) { this.now = frame0; this.started = true; }
    for (const id of Object.keys(blocks)) {
      if (!this.histories.has(id)) this.histories.set(id, new StreamHistory(this.historyFrames, frame0));
    }
    for (const [id, history] of this.histories) {
      const block = blocks[id];
      history.write(frame0, frames, block?.left ?? null, block?.right ?? null);
    }
    this.now = Math.max(this.now, frame0 + frames);
  }

  /** Starts (or stops, with null) the program on a stream without a splice. */
  setSource(id: string | null): void {
    if (id === null) {
      if (this.source) this.fadeOut = this.fadeFrames;
      this.pending = null;
      return;
    }
    if (!this.histories.has(id)) this.histories.set(id, new StreamHistory(this.historyFrames, this.now));
    this.source = id;
    this.pos = this.now - this.delayFrames;
    this.rate = 1;
    this.locked = true;
    this.pending = null;
    this.fadeIn = this.fadeFrames;
    this.fadeOut = 0;
  }

  /**
   * Schedules the handoff to `to`. `priorLagFrames` is the timestamp estimate
   * of the relay loop (null when unknown); it narrows the search and is the
   * fallback when the audio cannot be matched.
   */
  handoff(to: string, priorLagFrames: number | null = null): SplicePlan {
    const from = this.source;
    if (!from || this.fadeOut > 0) {
      this.setSource(to);
      return { status: "started", from, to, lagFrames: 0, confidence: 0, at: this.pos };
    }
    if (from === to) return { status: "same", from, to, lagFrames: 0, confidence: 0, at: this.pos };
    if (this.pending) this.finishSplice();
    if (!this.histories.has(to)) this.histories.set(to, new StreamHistory(this.historyFrames, this.now));

    let minLag = 0;
    let maxLag = this.maxLagFrames;
    if (priorLagFrames !== null && Number.isFinite(priorLagFrames)) {
      const slack = Math.round(this.sampleRate / 2);
      minLag = Math.max(0, Math.round(priorLagFrames) - slack);
      maxLag = Math.min(this.maxLagFrames, Math.round(priorLagFrames) + slack);
    }
    const span = this.windowFrames + maxLag + 1;
    const start = this.now - span;
    const fromHistory = this.histories.get(from)!;
    const toHistory = this.histories.get(to)!;
    const estimate = start >= fromHistory.end - fromHistory.capacity
      ? estimateLag(fromHistory.mono(start, span), toHistory.mono(start, span), { minLag, maxLag, window: this.windowFrames })
      : null;

    let status: SpliceStatus;
    let lag: number;
    let confidence = 0;
    if (estimate && estimate.confidence >= this.minConfidence) {
      status = "aligned";
      lag = estimate.lag;
      confidence = estimate.confidence;
    } else {
      status = "unaligned";
      lag = Math.max(0, Math.round(priorLagFrames ?? 0));
      confidence = estimate?.confidence ?? 0;
    }
    const at = Math.ceil(this.pos) + this.guardFrames;
    // B must be readable through at + lag + fade (+2 for the interpolator).
    const usable = this.now - (at + this.fadeFrames + 2);
    if (lag > usable) {
      lag = Math.max(0, usable);
      if (status === "aligned") status = "late";
    }
    this.pending = { to, at, lag, done: 0 };
    return { status, from, to, lagFrames: lag, confidence, at };
  }

  private finishSplice(): void {
    const p = this.pending!;
    this.source = p.to;
    this.pos += p.lag;
    this.pending = null;
    this.hold = 2 * this.sampleRate;
  }

  /** Renders `frames` program frames. Call once per ingested block. */
  render(frames: number): StereoBlock {
    const left = new Float32Array(frames);
    const right = new Float32Array(frames);
    if (!this.source) return { left, right };
    const a = this.scratch;
    const b = this.scratchB;
    for (let i = 0; i < frames; i += 1) {
      const history = this.histories.get(this.source!);
      if (!history) break;
      history.sample(this.pos, a);
      let l = a[0];
      let r = a[1];
      const p = this.pending;
      if (p && this.pos >= p.at) {
        this.histories.get(p.to)!.sample(this.pos + p.lag, b);
        const w = (p.done + 0.5) / this.fadeFrames;
        l = l * (1 - w) + b[0] * w;
        r = r * (1 - w) + b[1] * w;
        p.done += 1;
        if (p.done >= this.fadeFrames) this.finishSplice();
      }
      if (this.fadeIn > 0) {
        const w = 1 - this.fadeIn / this.fadeFrames;
        l *= w; r *= w;
        this.fadeIn -= 1;
      } else if (this.fadeOut > 0) {
        const w = this.fadeOut / this.fadeFrames;
        l *= w; r *= w;
        this.fadeOut -= 1;
        if (this.fadeOut === 0) { this.source = null; }
      }
      left[i] = l;
      right[i] = r;
      this.pos += this.rate;
      if (!this.source) break;
    }
    this.control(frames);
    return { left, right };
  }

  /** Delay-recovery loop; see the header. Runs once per rendered block. */
  private control(frames: number): void {
    // No rate change while splicing or for 2 s after: the splice region plays
    // sample-exact, and a pitch glide never coincides with the crossfade.
    if (this.pending) return;
    if (this.hold > 0) { this.hold -= frames; return; }
    const error = this.delay - this.delayFrames;
    if (this.locked && Math.abs(error) > this.deadbandFrames) this.locked = false;
    let target = 1;
    if (!this.locked) {
      // Full deviation beyond ~10 s of error per unit; slewed below.
      const deviation = error / (10 * this.sampleRate);
      target = 1 + Math.max(-this.maxRateDeviation, Math.min(this.maxRateDeviation, deviation));
      if (Math.abs(error) <= this.deadbandFrames / 2) {
        this.locked = true;
        target = 1;
      }
    }
    // Slew: reaching the full 0.2 % takes about 3 s, so pitch never steps.
    const step = (this.maxRateDeviation / (3 * this.sampleRate)) * frames;
    const next = this.rate + Math.max(-step, Math.min(step, target - this.rate));
    this.rate = this.locked && Math.abs(next - 1) <= step ? 1 : next;
    if (this.rate === 1) this.pos = Math.round(this.pos * 1e6) / 1e6;
  }
}
