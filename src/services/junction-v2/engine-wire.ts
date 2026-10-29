// Wire format of the engine's loopback PCM bridge (native junction::Link):
// 16-byte little-endian header {u8 kind, u8 channels(2), u16 reserved,
// u32 frames, f64 first frame index} + float32 interleaved stereo.
export const KIND_INPUT = 1;
export const KIND_PROGRAM = 2;
export const KIND_UPLINK = 3;
const HEADER = 16;

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
