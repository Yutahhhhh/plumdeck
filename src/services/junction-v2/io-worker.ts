// Junction v2 audio I/O worker. Everything on the audio path that is not inside
// the AudioWorklet or the engine runs here, never on the UI thread, so React
// renders and GC pauses cannot starve the JUNCTION deck or the program:
//
//   capture worklet --MessagePort--> [this worker] --ws--> engine
//        (all slots, one clock)        | program assembler (host): program -> engine program device
//                                      | own J feed slot           -> engine JUNCTION deck
//   engine uplink --ws--> [this worker] --MessagePort--> source worklet
//
// The UI thread only sends control (slot map, J feed, program source/handoff)
// and receives stats.
import { ProgramAssembler, type SplicePlan } from "./core/program.ts";
import { decodeBlock, encodeBlock, KIND_INPUT, KIND_PROGRAM, KIND_UPLINK } from "./engine-wire.ts";

export type IoCommand =
  | { t: "init"; capture: MessagePort; uplink: MessagePort; batch: number; program: boolean; delaySeconds: number }
  | { t: "connect"; port: number; token: string }
  | { t: "slots"; slots: (string | null)[] }
  | { t: "feed"; from: string | null }
  | { t: "source"; id: string | null }
  | { t: "handoff"; to: string; prior: number | null }
  | { t: "close" };

export type IoEvent =
  | { t: "socket"; open: boolean }
  | { t: "splice"; plan: SplicePlan }
  | { t: "stats"; programDelayFrames: number | null; captureFrames: number; uplinkFrames: number };

declare const self: { postMessage(message: IoEvent): void; onmessage: ((event: MessageEvent<IoCommand>) => void) | null };

let socket: WebSocket | null = null;
let program: ProgramAssembler | null = null;
let slots: (string | null)[] = [];
let feed: string | null = null;
let batch = 1024;
let uplinkPort: MessagePort | null = null;
let captureFrames = 0;
let uplinkFrames = 0;
let closed = false;
let statsTimer: ReturnType<typeof setInterval> | null = null;

function send(buffer: ArrayBuffer) {
  if (!socket || socket.readyState !== WebSocket.OPEN || socket.bufferedAmount > 2_000_000) return;
  socket.send(buffer);
}

function onCapture({ frame, frames, data }: { frame: number; frames: number; data: Float32Array }) {
  captureFrames += frames;
  const blocks: Record<string, { left: Float32Array; right: Float32Array }> = {};
  slots.forEach((id, slot) => {
    if (!id) return;
    const left = new Float32Array(frames), right = new Float32Array(frames);
    const base = slot * 2 * batch;
    for (let i = 0; i < frames; i += 1) { left[i] = data[base + 2 * i]; right[i] = data[base + 2 * i + 1]; }
    blocks[id] = { left, right };
  });
  if (program) {
    program.ingest(frame, frames, blocks);
    const out = program.render(frames);
    send(encodeBlock(KIND_PROGRAM, frame, out.left, out.right));
  }
  const own = feed ? blocks[feed] : undefined;
  if (own) send(encodeBlock(KIND_INPUT, frame, own.left, own.right));
}

function connect(port: number, token: string) {
  socket?.close();
  const ws = new WebSocket(`ws://127.0.0.1:${port}/jlink?token=${encodeURIComponent(token)}`);
  ws.binaryType = "arraybuffer";
  socket = ws;
  ws.onopen = () => self.postMessage({ t: "socket", open: true });
  ws.onclose = () => { if (socket === ws) { socket = null; if (!closed) self.postMessage({ t: "socket", open: false }); } };
  ws.onmessage = (event) => {
    if (!(event.data instanceof ArrayBuffer)) return;
    const block = decodeBlock(event.data);
    if (block?.kind !== KIND_UPLINK || !uplinkPort) return;
    uplinkFrames += block.left.length;
    uplinkPort.postMessage({ left: block.left, right: block.right }, [block.left.buffer, block.right.buffer]);
  };
}

self.onmessage = (event) => {
  const cmd = event.data;
  switch (cmd.t) {
    case "init":
      batch = cmd.batch;
      uplinkPort = cmd.uplink;
      program = cmd.program ? new ProgramAssembler({ delaySeconds: cmd.delaySeconds }) : null;
      cmd.capture.onmessage = (e: MessageEvent<{ frame: number; frames: number; data: Float32Array }>) => onCapture(e.data);
      statsTimer = setInterval(() => self.postMessage({ t: "stats", programDelayFrames: program?.source ? program.delay : null, captureFrames, uplinkFrames }), 1000);
      return;
    case "connect": connect(cmd.port, cmd.token); return;
    case "slots": slots = cmd.slots; return;
    case "feed": feed = cmd.from; return;
    case "source": program?.setSource(cmd.id); return;
    case "handoff": if (program) self.postMessage({ t: "splice", plan: program.handoff(cmd.to, cmd.prior) }); return;
    case "close":
      closed = true;
      socket?.close();
      socket = null;
      if (statsTimer) clearInterval(statsTimer);
      return;
  }
};
