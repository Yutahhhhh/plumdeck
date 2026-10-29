// WebView side of the engine's loopback PCM bridge (native junction::Link).
// Control goes through the normal NDJSON engine session ("jlink.*" ops);
// audio goes over a binary WebSocket to 127.0.0.1 with a per-start token.
import { djEngineClient } from "@/services/dj-engine/client";
import type { JunctionInputChannel, JunctionInputSettings } from "@/types/junction";

const KIND_INPUT = 1;
const KIND_PROGRAM = 2;
const KIND_UPLINK = 3;
const HEADER = 16;

export type UplinkMode = "full" | "local" | "off";

export interface EngineLinkState {
  active: boolean;
  port: number;
  token: string;
  connected: boolean;
  uplink: UplinkMode;
  input: { receiving: boolean; latencyMs: number; underruns: number; realignments: number };
  deck: JunctionInputChannel & { mainMix?: boolean };
  program: { open: boolean; deviceId: string | null; peak?: number; underruns?: number; latencyMs?: number; driftPpm?: number };
  frames: Record<string, number>;
}

export function encodeBlock(kind: number, frameIndex: number, left: Float32Array, right: Float32Array): ArrayBuffer {
  const frames = left.length;
  const buffer = new ArrayBuffer(HEADER + frames * 8);
  const view = new DataView(buffer);
  view.setUint8(0, kind);
  view.setUint8(1, 2);
  view.setUint32(4, frames, true);
  view.setFloat64(8, frameIndex, true);
  const pcm = new Float32Array(buffer, HEADER, frames * 2);
  for (let i = 0; i < frames; i += 1) { pcm[2 * i] = left[i]; pcm[2 * i + 1] = right[i]; }
  return buffer;
}

export function decodeBlock(buffer: ArrayBuffer): { kind: number; frameIndex: number; left: Float32Array; right: Float32Array } | null {
  if (buffer.byteLength < HEADER) return null;
  const view = new DataView(buffer);
  const kind = view.getUint8(0);
  const frames = view.getUint32(4, true);
  if (view.getUint8(1) !== 2 || buffer.byteLength !== HEADER + frames * 8) return null;
  const pcm = new Float32Array(buffer, HEADER, frames * 2);
  const left = new Float32Array(frames), right = new Float32Array(frames);
  for (let i = 0; i < frames; i += 1) { left[i] = pcm[2 * i]; right[i] = pcm[2 * i + 1]; }
  return { kind, frameIndex: view.getFloat64(8, true), left, right };
}

export class EngineLink {
  private socket: WebSocket | null = null;
  private closed = false;
  onUplink: ((frameIndex: number, left: Float32Array, right: Float32Array) => void) | null = null;
  lastState: EngineLinkState | null = null;

  private async op(name: string, params: Record<string, unknown> = {}): Promise<EngineLinkState> {
    const state = (await djEngineClient.send(`jlink.${name}`, params)) as EngineLinkState;
    this.lastState = state;
    return state;
  }

  async start(): Promise<void> {
    const state = await this.op("start");
    await this.connect(state);
  }

  private connect(state: EngineLinkState): Promise<void> {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(`ws://127.0.0.1:${state.port}/jlink?token=${encodeURIComponent(state.token)}`);
      socket.binaryType = "arraybuffer";
      socket.onopen = () => { this.socket = socket; resolve(); };
      socket.onerror = () => reject(new Error("エンジンとの音声経路に接続できませんでした"));
      socket.onmessage = (event) => {
        if (!(event.data instanceof ArrayBuffer)) return;
        const block = decodeBlock(event.data);
        if (block?.kind === KIND_UPLINK) this.onUplink?.(block.frameIndex, block.left, block.right);
      };
      socket.onclose = () => {
        if (this.socket === socket) this.socket = null;
        // The engine restarted or the socket dropped: reconnect with a fresh token.
        if (!this.closed) setTimeout(() => { if (!this.closed) void this.op("start").then((s) => this.connect(s)).catch(() => undefined); }, 1000);
      };
    });
  }

  private sendRaw(buffer: ArrayBuffer) {
    const socket = this.socket;
    if (!socket || socket.readyState !== WebSocket.OPEN) return;
    // A stalled engine must not grow memory: drop instead of queueing seconds.
    if (socket.bufferedAmount > 2_000_000) return;
    socket.send(buffer);
  }

  sendInput(frameIndex: number, left: Float32Array, right: Float32Array) { this.sendRaw(encodeBlock(KIND_INPUT, frameIndex, left, right)); }
  sendProgram(frameIndex: number, left: Float32Array, right: Float32Array) { this.sendRaw(encodeBlock(KIND_PROGRAM, frameIndex, left, right)); }

  setUplink(mode: UplinkMode) { return this.op("uplink", { mode }); }
  setInput(settings: JunctionInputSettings) { return this.op("input.set", { ...settings }); }
  openProgram(deviceId: string, gain = 1) { return this.op("program.open", { deviceId, gain }); }
  closeProgram() { return this.op("program.close"); }
  state() { return this.op("state"); }

  async stop(): Promise<void> {
    this.closed = true;
    this.socket?.close();
    this.socket = null;
    await this.op("stop").catch(() => undefined);
  }
}
