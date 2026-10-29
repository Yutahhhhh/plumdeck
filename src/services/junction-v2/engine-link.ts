// WebView side of the engine's loopback PCM bridge (native junction::Link).
// Control goes through the normal NDJSON engine session ("jlink.*" ops); the
// binary WebSocket with the per-start token is opened by io-worker.ts.
import { djEngineClient } from "@/services/dj-engine/client";
import type { JunctionInputChannel, JunctionInputSettings } from "@/types/junction";

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

/** NDJSON control of the engine's Junction v2 bridge. The PCM socket itself
 * lives in the I/O worker (io-worker.ts). */
export class EngineLink {
  lastState: EngineLinkState | null = null;

  private async op(name: string, params: Record<string, unknown> = {}): Promise<EngineLinkState> {
    const state = (await djEngineClient.send(`jlink.${name}`, params)) as EngineLinkState;
    this.lastState = state;
    return state;
  }

  start() { return this.op("start"); }
  setUplink(mode: UplinkMode) { return this.op("uplink", { mode }); }
  setInput(settings: JunctionInputSettings) { return this.op("input.set", { ...settings }); }
  openProgram(deviceId: string, gain = 1) { return this.op("program.open", { deviceId, gain }); }
  closeProgram() { return this.op("program.close"); }
  state() { return this.op("state"); }
  async stop(): Promise<void> { await this.op("stop").catch(() => undefined); }
}
