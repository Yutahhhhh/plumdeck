// The I/O worker glue: capture batches -> program + own J feed -> engine wire,
// engine uplink -> source worklet port. Runs the real module with a fake
// WebSocket and node's MessageChannel.
import assert from "node:assert/strict";
import test from "node:test";
import { decodeBlock, encodeBlock, KIND_INPUT, KIND_PROGRAM, KIND_UPLINK } from "./engine-wire.ts";

const events = [];
const sockets = [];
globalThis.self = { postMessage: (m) => events.push(m), onmessage: null };
globalThis.WebSocket = class {
  static OPEN = 1;
  constructor(url) { this.url = url; this.readyState = 1; this.bufferedAmount = 0; this.sent = []; sockets.push(this); queueMicrotask(() => this.onopen?.()); }
  send(buffer) { this.sent.push(buffer); }
  close() { this.readyState = 3; }
};
await import("./io-worker.ts");
const flush = async () => { for (let i = 0; i < 20; i += 1) await new Promise((r) => setImmediate(r)); };
const cmd = (c, transfer = []) => self.onmessage({ data: c, ports: transfer });

test("capture slots become program and J input on the engine wire; uplink reaches the source port", async () => {
  const capture = new MessageChannel();
  const uplink = new MessageChannel();
  const batch = 1024;
  const delaySeconds = 0.1; // 4800 frames
  cmd({ t: "init", capture: capture.port2, uplink: uplink.port2, batch, program: true, delaySeconds });
  cmd({ t: "connect", port: 4242, token: "tok en" });
  assert.equal(sockets[0].url, "ws://127.0.0.1:4242/jlink?token=tok%20en");
  cmd({ t: "slots", slots: ["host", "g1"] });
  cmd({ t: "feed", from: "g1" });
  cmd({ t: "source", id: "host" });
  const a = (n) => Math.sin(n / 7), b = (n) => Math.cos(n / 11);
  for (let k = 0; k < 10; k += 1) {
    const data = new Float32Array(2 * 2 * batch);
    for (let i = 0; i < batch; i += 1) {
      const n = k * batch + i;
      data[2 * i] = a(n); data[2 * i + 1] = a(n);
      data[2 * batch + 2 * i] = b(n); data[2 * batch + 2 * i + 1] = -b(n);
    }
    capture.port1.postMessage({ frame: k * batch, frames: batch, data });
  }
  await flush();
  const blocks = sockets[0].sent.map((buffer) => decodeBlock(buffer));
  const inputs = blocks.filter((x) => x.kind === KIND_INPUT);
  const programs = blocks.filter((x) => x.kind === KIND_PROGRAM);
  assert.equal(inputs.length, 10);
  assert.equal(programs.length, 10);
  assert.equal(inputs[3].frameIndex, 3 * batch);
  for (let i = 0; i < batch; i += 1) {
    assert.ok(Math.abs(inputs[3].left[i] - b(3 * batch + i)) < 1e-6);
    assert.ok(Math.abs(inputs[3].right[i] + b(3 * batch + i)) < 1e-6);
  }
  // Program plays the host slot 4800 frames late (after a 30 ms fade-in).
  const program = new Float32Array(10 * batch);
  programs.forEach((p) => program.set(p.left, p.frameIndex));
  for (let n = 4800 + 1440; n < 10 * batch; n += 1) assert.ok(Math.abs(program[n] - a(n - 4800)) < 1e-5, `frame ${n}`);
  for (let n = 0; n < 4800; n += 1) assert.equal(program[n], 0);

  const received = new Promise((resolve) => { uplink.port1.onmessage = (e) => resolve(e.data); });
  const left = Float32Array.from([0.1, 0.2, 0.3]), right = Float32Array.from([-0.1, -0.2, -0.3]);
  sockets[0].onmessage({ data: encodeBlock(KIND_UPLINK, 0, left, right) });
  const got = await received;
  assert.deepEqual(Array.from(got.left), Array.from(left));
  assert.deepEqual(Array.from(got.right), Array.from(right));

  cmd({ t: "handoff", to: "g1", prior: null });
  assert.equal(events.at(-1).t, "splice");
  cmd({ t: "close" });
  capture.port1.close(); uplink.port1.close();
});
