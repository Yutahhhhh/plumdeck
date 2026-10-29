// AudioWorklet processors for Junction v2. Loaded with `?worker&url` so Vite
// bundles the imports into one classic script.
//
//  jlink-capture  N inputs -> batches of interleaved stereo posted (to the
//                 I/O worker's MessagePort once handed over) with the
//                 AudioContext frame of their first sample.
//                 All inputs share that frame, which is what makes the
//                 program splice a pure index computation (see program.ts).
//  jlink-source   PCM pushed by the I/O worker (the engine's uplink, on the
//                 engine's clock) -> elastic buffer -> stereo output on the
//                 AudioContext clock.
import { ElasticBuffer } from "./core/elastic.ts";

declare const currentFrame: number;
declare function registerProcessor(name: string, ctor: unknown): void;
declare class AudioWorkletProcessor {
  readonly port: MessagePort;
  constructor(options?: unknown);
}

interface CaptureOptions { slots: number; batch: number }

class CaptureProcessor extends AudioWorkletProcessor {
  private readonly slots: number;
  private readonly batch: number;
  private buffer: Float32Array;
  private filled = 0;
  private start = 0;
  private out: MessagePort | null = null;

  constructor(options: { processorOptions?: CaptureOptions }) {
    super(options);
    this.slots = options.processorOptions?.slots ?? 1;
    this.batch = options.processorOptions?.batch ?? 1024;
    this.buffer = new Float32Array(this.slots * 2 * this.batch);
    this.port.onmessage = (event: MessageEvent<{ port?: MessagePort }>) => { if (event.data.port) this.out = event.data.port; };
  }

  process(inputs: Float32Array[][]): boolean {
    const frames = inputs[0]?.[0]?.length ?? 128;
    if (this.filled === 0) this.start = currentFrame;
    for (let s = 0; s < this.slots; s += 1) {
      const input = inputs[s] ?? [];
      const left = input[0];
      const right = input[1] ?? left;
      const base = s * 2 * this.batch;
      for (let i = 0; i < frames; i += 1) {
        this.buffer[base + 2 * (this.filled + i)] = left ? left[i] : 0;
        this.buffer[base + 2 * (this.filled + i) + 1] = right ? right[i] : 0;
      }
    }
    this.filled += frames;
    if (this.filled >= this.batch) {
      (this.out ?? this.port).postMessage({ frame: this.start, frames: this.filled, data: this.buffer }, [this.buffer.buffer]);
      this.buffer = new Float32Array(this.slots * 2 * this.batch);
      this.filled = 0;
    }
    return true;
  }
}

class SourceProcessor extends AudioWorkletProcessor {
  private readonly elastic = new ElasticBuffer({ targetFill: 4096, capacity: 96000 });
  private readonly l = new Float32Array(128);
  private readonly r = new Float32Array(128);
  private quanta = 0;

  constructor(options: unknown) {
    super(options);
    const push = (event: MessageEvent<{ left: Float32Array; right: Float32Array }>) => this.elastic.push(event.data.left, event.data.right);
    this.port.onmessage = (event: MessageEvent<{ port?: MessagePort; left: Float32Array; right: Float32Array }>) => {
      if (event.data.port) event.data.port.onmessage = push;
      else push(event);
    };
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const out = outputs[0];
    const frames = out[0]?.length ?? 128;
    this.elastic.pull(this.l, this.r, frames);
    out[0]?.set(this.l.subarray(0, frames));
    (out[1] ?? out[0])?.set(this.r.subarray(0, frames));
    this.quanta += 1;
    if (this.quanta % 375 === 0) {
      this.port.postMessage({ fill: this.elastic.fill, underruns: this.elastic.underruns, skips: this.elastic.skips, ratio: this.elastic.currentRatio });
    }
    return true;
  }
}

registerProcessor("jlink-capture", CaptureProcessor);
registerProcessor("jlink-source", SourceProcessor);
