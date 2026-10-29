// Elastic buffer between two audio clocks (the native engine's sound card and
// the WebView's AudioContext). The producer pushes whatever arrives; the
// consumer pulls exactly what its render quantum needs. A slow proportional
// loop on the fill level nudges the read rate (cubic Hermite interpolation)
// so the buffer neither drains nor grows; underruns fade to silence and
// re-prime, floods skip forward. Used inside the AudioWorklet, so it must not
// allocate after construction.
import { hermite } from "./program.ts";

export interface ElasticOptions {
  capacity?: number;
  targetFill?: number;
  maxRatioDeviation?: number;
}

export class ElasticBuffer {
  private readonly capacity: number;
  private readonly l: Float32Array;
  private readonly r: Float32Array;
  private write = 0;
  /** Fractional read position, absolute frames. */
  private read = 0;
  private primed = false;
  private ratio = 1;
  private fade = 0;
  readonly targetFill: number;
  private readonly maxDev: number;
  underruns = 0;
  skips = 0;

  constructor(options: ElasticOptions = {}) {
    this.capacity = options.capacity ?? 48000;
    this.targetFill = options.targetFill ?? 3072;
    this.maxDev = options.maxRatioDeviation ?? 0.004;
    this.l = new Float32Array(this.capacity);
    this.r = new Float32Array(this.capacity);
  }

  get fill(): number { return this.write - this.read; }
  get currentRatio(): number { return this.ratio; }

  push(left: Float32Array, right: Float32Array, frames = left.length): void {
    for (let i = 0; i < frames; i += 1) {
      const slot = (this.write + i) % this.capacity;
      this.l[slot] = left[i];
      this.r[slot] = right[i];
    }
    this.write += frames;
    // Never let the writer lap the reader: drop the oldest audio instead.
    if (this.write - this.read > this.capacity - 8) {
      this.read = this.write - this.targetFill;
      this.skips += 1;
    }
  }

  private at(buffer: Float32Array, frame: number): number {
    return frame < this.write ? buffer[((frame % this.capacity) + this.capacity) % this.capacity] : 0;
  }

  pull(outL: Float32Array, outR: Float32Array, frames = outL.length): void {
    if (!this.primed) {
      if (this.fill >= this.targetFill) {
        this.primed = true;
        this.fade = 0;
        this.read = this.write - this.targetFill;
      } else {
        outL.fill(0, 0, frames);
        outR.fill(0, 0, frames);
        return;
      }
    }
    // Way too much buffered (e.g. the tab was frozen): jump close to target.
    if (this.fill > this.targetFill * 4) {
      this.read = this.write - this.targetFill;
      this.skips += 1;
    }
    const error = (this.fill - this.targetFill) / this.targetFill;
    const target = 1 + Math.max(-this.maxDev, Math.min(this.maxDev, error * 0.002));
    this.ratio += (target - this.ratio) * 0.05;
    for (let i = 0; i < frames; i += 1) {
      if (this.read + 3 >= this.write) {
        // Starved: ramp the rest of this quantum to zero and re-prime.
        const lastL = i > 0 ? outL[i - 1] : 0;
        const lastR = i > 0 ? outR[i - 1] : 0;
        for (let k = i; k < frames; k += 1) {
          const w = Math.max(0, 1 - (k - i + 1) / 32);
          outL[k] = lastL * w;
          outR[k] = lastR * w;
        }
        this.primed = false;
        this.underruns += 1;
        return;
      }
      const base = Math.floor(this.read);
      const t = this.read - base;
      let l = hermite(this.at(this.l, base - 1), this.at(this.l, base), this.at(this.l, base + 1), this.at(this.l, base + 2), t);
      let r = hermite(this.at(this.r, base - 1), this.at(this.r, base), this.at(this.r, base + 1), this.at(this.r, base + 2), t);
      if (this.fade < 64) {
        const w = this.fade / 64;
        l *= w; r *= w;
        this.fade += 1;
      }
      outL[i] = l;
      outR[i] = r;
      this.read += this.ratio;
    }
  }
}
