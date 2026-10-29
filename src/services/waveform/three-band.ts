/**
 * The one 3-band waveform model and its rekordbox-like renderer.
 *
 * Every surface (deck overview, deck zoomed lane, browser previews, mini
 * player) draws a `ThreeBandWave`: low, mid and high per column on a 0..scale
 * axis. The backend fills it from rekordbox PWV6 (overview) / PWV7 (detail),
 * already shifted onto the playback timeline, or from plumdeck's own decoded
 * bands; a shared Junction asset converts its band peaks into the same shape.
 * Pure module apart from the canvas calls: runs under `node --test`.
 */

/** Colours sampled from the owner's rekordbox 3Band view. */
export const WAVE3_COLORS = { low: "#2048D8", mid: "#F0A038", high: "#F0E8D8", overview: "#2050D8" } as const;

export type ThreeBandLevel = "overview" | "detail";

export interface ThreeBandWave {
  source: "rekordbox" | "own" | "shared";
  columns: number;
  /** Columns per second for a detail wave; null for an overview spanning `durationMs`. */
  columnsPerSecond: number | null;
  /** Playback time of column 0 (rekordbox data starts at −offset). */
  startMs: number;
  durationMs: number;
  scale: number;
  /** Interleaved low, mid, high. */
  data: Uint8Array;
  /** Display gain: the loudest 2 % of columns reach ~95 % of the lane. */
  gain: number;
}

export interface ThreeBandPayload {
  source: string; columns: number; columns_per_second: number | null; start_ms: number; duration_ms: number; scale: number; data: string;
}

function decodeBase64(text: string): Uint8Array {
  if (typeof atob === "function") {
    const binary = atob(text);
    const bytes = new Uint8Array(binary.length);
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
    return bytes;
  }
  return new Uint8Array((globalThis as unknown as { Buffer: { from(t: string, e: string): Uint8Array } }).Buffer.from(text, "base64"));
}

/** Gain from the 98th percentile of the loudest band per column (1..3). */
export function displayGain(data: Uint8Array, scale: number): number {
  const histogram = new Uint32Array(256);
  const columns = Math.floor(data.length / 3);
  for (let c = 0; c < columns; c++) histogram[Math.max(data[c * 3], data[c * 3 + 1], data[c * 3 + 2])]++;
  let remaining = Math.floor(columns * 0.02), top = 255;
  for (; top > 1; top--) { remaining -= histogram[top]; if (remaining < 0) break; }
  return Math.min(3, Math.max(1, (scale * 0.95) / Math.max(1, top)));
}

export function parseThreeBand(payload: ThreeBandPayload): ThreeBandWave | null {
  if (!payload || typeof payload.data !== "string" || !(payload.columns > 0) || !(payload.scale > 0) || !Number.isFinite(payload.start_ms) || !(payload.duration_ms > 0)) return null;
  const data = decodeBase64(payload.data);
  if (data.length < payload.columns * 3) return null;
  const source = payload.source === "rekordbox" ? "rekordbox" : "own";
  return {
    source, columns: payload.columns, columnsPerSecond: payload.columns_per_second && payload.columns_per_second > 0 ? payload.columns_per_second : null,
    startMs: payload.start_ms, durationMs: payload.duration_ms, scale: payload.scale, data, gain: displayGain(data, payload.scale),
  };
}

/** A shared (Junction) asset's band peaks as the same model. */
export function fromBandPeaks(value: { bins_per_second: number; duration_ms: number; amplitude_scale: number; low: number[]; mid: number[]; high: number[] }): ThreeBandWave {
  const columns = value.low.length, data = new Uint8Array(columns * 3);
  const tilt = [1, 0.85, 0.5], to = (v: number, band: number) => Math.max(0, Math.min(127, Math.round(v / value.amplitude_scale * 127 * tilt[band])));
  for (let c = 0; c < columns; c++) { data[c * 3] = to(value.low[c], 0); data[c * 3 + 1] = to(value.mid[c], 1); data[c * 3 + 2] = to(value.high[c], 2); }
  return { source: "shared", columns, columnsPerSecond: value.bins_per_second, startMs: 0, durationMs: value.duration_ms, scale: 127, data, gain: displayGain(data, 127) };
}

/** Fractional column of a playback time. */
export function columnAt(wave: ThreeBandWave, timeMs: number): number {
  return wave.columnsPerSecond ? (timeMs - wave.startMs) * wave.columnsPerSecond / 1000 : (timeMs - wave.startMs) / wave.durationMs * wave.columns;
}

/**
 * Low / mid / high (0..1, gain applied) for `pixels` columns covering
 * [startMs, startMs + spanMs). Peaks over a pixel when it covers several
 * columns; linear interpolation when zoomed past one column per pixel.
 */
export function sampleThreeBand(wave: ThreeBandWave, startMs: number, spanMs: number, pixels: number, out?: { low: Float32Array; mid: Float32Array; high: Float32Array }) {
  const target = out && out.low.length >= pixels ? out : { low: new Float32Array(pixels), mid: new Float32Array(pixels), high: new Float32Array(pixels) };
  const { low, mid, high } = target;
  const k = wave.gain / wave.scale, data = wave.data, last = wave.columns - 1;
  const step = spanMs / pixels;
  for (let x = 0; x < pixels; x++) {
    const from = columnAt(wave, startMs + x * step), to = columnAt(wave, startMs + (x + 1) * step);
    let l = 0, m = 0, h = 0;
    if (to < 0 || from > last) { low[x] = mid[x] = high[x] = 0; continue; }
    if (to - from < 1) {
      const c = Math.max(0, Math.min(last, Math.floor(from))), n = Math.min(last, c + 1), t = Math.max(0, Math.min(1, from - c));
      l = data[c * 3] + (data[n * 3] - data[c * 3]) * t;
      m = data[c * 3 + 1] + (data[n * 3 + 1] - data[c * 3 + 1]) * t;
      h = data[c * 3 + 2] + (data[n * 3 + 2] - data[c * 3 + 2]) * t;
    } else {
      const a = Math.max(0, Math.floor(from)), b = Math.min(last, Math.ceil(to) - 1);
      for (let c = a; c <= b; c++) {
        if (data[c * 3] > l) l = data[c * 3];
        if (data[c * 3 + 1] > m) m = data[c * 3 + 1];
        if (data[c * 3 + 2] > h) h = data[c * 3 + 2];
      }
    }
    low[x] = Math.min(1, l * k); mid[x] = Math.min(1, m * k); high[x] = Math.min(1, h * k);
  }
  return target;
}

export interface DrawOptions {
  startMs: number; spanMs: number; width: number; height: number;
  vertical?: boolean; side?: "left" | "right";
  /** overview: stacked bands on a blue base; detail: overlaid bands. */
  style: ThreeBandLevel;
  /** Part of the lane kept free (beat ruler, phrase strip), in px. */
  reserve?: number;
  /** Device pixel ratio already applied to the context. */
  ratio?: number;
}

const scratch = { low: new Float32Array(0), mid: new Float32Array(0), high: new Float32Array(0) };

/** Draws the waveform; returns false when nothing of it is in view. */
export function drawThreeBand(ctx: CanvasRenderingContext2D, wave: ThreeBandWave, options: DrawOptions): boolean {
  const vertical = Boolean(options.vertical), side = options.side ?? "left";
  const length = vertical ? options.height : options.width, breadth = vertical ? options.width : options.height;
  const pixels = Math.max(1, Math.ceil(length * (options.ratio ?? 1)));
  if (scratch.low.length < pixels) { const n = Math.ceil(pixels / 256) * 256; scratch.low = new Float32Array(n); scratch.mid = new Float32Array(n); scratch.high = new Float32Array(n); }
  const { low, mid, high } = sampleThreeBand(wave, options.startMs, options.spanMs, pixels, scratch);
  const axis = vertical ? (side === "left" ? options.width - 2 : 2) : options.height / 2;
  const half = Math.max(0, breadth - (options.reserve ?? 0)) * (vertical ? 1 : 0.5);
  const unit = length / pixels;
  const front = vertical ? (side === "left" ? -1 : 1) : -1;
  const fill = (color: string, value: (i: number) => number, alpha = 1) => {
    ctx.globalAlpha = alpha; ctx.fillStyle = color; ctx.beginPath();
    if (vertical) ctx.moveTo(axis, 0); else ctx.moveTo(0, axis);
    for (let i = 0; i < pixels; i++) {
      const size = value(i) * half * front, at = i * unit;
      if (vertical) { ctx.lineTo(axis + size, at); ctx.lineTo(axis + size, at + unit); }
      else { ctx.lineTo(at, axis + size); ctx.lineTo(at + unit, axis + size); }
    }
    if (vertical) ctx.lineTo(axis, length);
    else for (let i = pixels - 1; i >= 0; i--) { const size = value(i) * half, at = i * unit; ctx.lineTo(at + unit, axis + size); ctx.lineTo(at, axis + size); }
    ctx.closePath(); ctx.fill(); ctx.globalAlpha = 1;
  };
  if (options.style === "overview") {
    // Stacked: the blue base is the sum, amber the mid + high part, cream the high part.
    // rekordbox's overview reads mostly blue with an amber body and a thin
    // cream core, so the upper bands are weighted down in the stack.
    const total = (i: number) => Math.min(1, (low[i] + mid[i] + high[i]) / 2.2);
    fill(WAVE3_COLORS.overview, total);
    fill(WAVE3_COLORS.mid, (i) => Math.min(total(i), (mid[i] + high[i] * 0.5) / 2.2));
    fill(WAVE3_COLORS.high, (i) => Math.min(total(i), high[i] * 0.35 / 2.2));
  } else {
    // Overlaid: low blue, mid amber over it (brown where they overlap), high cream last.
    fill(WAVE3_COLORS.low, (i) => low[i]);
    fill(WAVE3_COLORS.mid, (i) => mid[i], 0.82);
    fill(WAVE3_COLORS.high, (i) => high[i] * 0.9);
  }
  return true;
}

// ------------------------------------------------------------- phrases

export type PhraseKind = "intro" | "verse" | "bridge" | "chorus" | "up" | "down" | "outro";
export interface Phrase { startMs: number; endMs: number; startBeat: number; endBeat: number; kind: PhraseKind; label: string; fillMs: number | null }

/** Own colour tokens with rekordbox's meaning per phrase family. */
export const PHRASE_COLORS: Record<PhraseKind, string> = {
  intro: "#3d7be0", up: "#e8973a", down: "#7b5cd6", chorus: "#e0455c", verse: "#5aa86b", bridge: "#d6c34a", outro: "#5c7d9e",
};

export function parsePhrases(value: unknown): Phrase[] {
  const items = (value as { items?: unknown[] } | null)?.items;
  if (!Array.isArray(items)) return [];
  return items.flatMap((raw) => {
    const item = raw as Record<string, unknown>;
    const kind = item.kind as PhraseKind;
    if (!(kind in PHRASE_COLORS) || typeof item.start_ms !== "number" || typeof item.end_ms !== "number" || item.end_ms <= item.start_ms) return [];
    return [{ startMs: item.start_ms, endMs: item.end_ms, startBeat: Number(item.start_beat) || 0, endBeat: Number(item.end_beat) || 0, kind,
      label: typeof item.label === "string" ? item.label : kind, fillMs: typeof item.fill_ms === "number" ? item.fill_ms : null }];
  });
}

/** The phrase under `positionMs` and the next one. */
export function phraseAt(phrases: readonly Phrase[], positionMs: number): { current: Phrase | null; next: Phrase | null } {
  let current: Phrase | null = null, next: Phrase | null = null;
  for (const phrase of phrases) {
    if (phrase.startMs <= positionMs && positionMs < phrase.endMs) current = phrase;
    else if (phrase.startMs > positionMs) { next = phrase; break; }
  }
  return { current, next };
}

/**
 * Whole bars from `positionMs` to the next phrase boundary, counted on the
 * deck's beat grid (beat times) or a constant grid. Null without a grid or
 * a next phrase.
 */
export function barsToNextPhrase(phrases: readonly Phrase[], positionMs: number, grid: { beatTimesMs?: number[]; bpm?: number | null; beatgridOffsetMs?: number; beatsPerBar?: number }): number | null {
  const { next } = phraseAt(phrases, positionMs);
  if (!next) return null;
  const meter = grid.beatsPerBar && grid.beatsPerBar > 0 ? grid.beatsPerBar : 4;
  let beats: number;
  if (grid.beatTimesMs && grid.beatTimesMs.length > 1) {
    const index = (ms: number) => { let lo = 0, hi = grid.beatTimesMs!.length; while (lo < hi) { const m = (lo + hi) >>> 1; if (grid.beatTimesMs![m] < ms) lo = m + 1; else hi = m; } return lo; };
    beats = index(next.startMs - 15) - index(positionMs + 1);
  } else if (grid.bpm && grid.bpm > 0) {
    beats = Math.ceil((next.startMs - positionMs - 15) / (60_000 / grid.bpm));
  } else return null;
  return Math.max(0, Math.ceil(beats / meter));
}
