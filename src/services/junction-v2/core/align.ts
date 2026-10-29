// Lag measurement between two captured streams, used to splice the program.
//
// Problem: the next DJ's uplink `mix` contains the on-air audio `ref` as the
// next DJ heard it on their J deck, i.e. delayed by the whole loop
//   host relay -> network -> guest jitter buffer -> J deck -> guest mix
//   -> network -> host jitter buffer
// so mix[t] ≈ g·ref[t − L] + (the next DJ's own decks). We need L to the sample.
//
// Why not timestamps alone: WebRTC receivers time-stretch audio inside the
// jitter buffer (NetEq accelerate / preemptive expand) and browsers expose no
// per-sample media clock, so a timestamp attached to a packet does not say
// which output sample it became. Timestamps (RTT plus the guest's reported
// buffer latencies) are only good to tens of milliseconds; we use them as a
// prior that narrows the search window. The audio itself is the ground truth:
//
//  1. Coarse: decimate both signals (box filter, factor d) and compute the
//     generalized cross-correlation with partial phase transform (GCC-PHAT,
//     |R|^-β weighting). Phase whitening makes the peak sharp even when the
//     next DJ's own track is as loud as the J feed and both are bass-heavy.
//  2. Fine: around the coarse peak (±2d samples) evaluate the normalized
//     cross-correlation of the first differences at full rate and take the
//     integer-sample maximum.
//  3. Confidence = that normalized correlation. When the next DJ has the J
//     deck faded out there is no `ref` in `mix`; confidence collapses and the
//     caller falls back to the prior (nothing to align, nothing to echo).
//
// Pure functions over Float32Arrays; deterministic.

/** In-place iterative radix-2 FFT. `re.length` must be a power of two. */
export function fft(re: Float64Array, im: Float64Array, inverse = false): void {
  const n = re.length;
  for (let i = 1, j = 0; i < n; i += 1) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) {
      let t = re[i]; re[i] = re[j]; re[j] = t;
      t = im[i]; im[i] = im[j]; im[j] = t;
    }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const angle = ((inverse ? 2 : -2) * Math.PI) / len;
    const wr = Math.cos(angle);
    const wi = Math.sin(angle);
    const half = len >> 1;
    for (let i = 0; i < n; i += len) {
      let cr = 1;
      let ci = 0;
      for (let k = 0; k < half; k += 1) {
        const a = i + k;
        const b = a + half;
        const xr = re[b] * cr - im[b] * ci;
        const xi = re[b] * ci + im[b] * cr;
        re[b] = re[a] - xr; im[b] = im[a] - xi;
        re[a] += xr; im[a] += xi;
        const nr = cr * wr - ci * wi;
        ci = cr * wi + ci * wr;
        cr = nr;
      }
    }
  }
  if (inverse) for (let i = 0; i < n; i += 1) { re[i] /= n; im[i] /= n; }
}

function decimate(input: Float32Array, start: number, length: number, factor: number): Float64Array {
  const out = new Float64Array(Math.floor(length / factor));
  for (let i = 0; i < out.length; i += 1) {
    let sum = 0;
    const base = start + i * factor;
    for (let k = 0; k < factor; k += 1) sum += input[base + k];
    out[i] = sum / factor;
  }
  return out;
}

export interface LagOptions {
  /** Search range in frames, inclusive: mix[t] ≈ ref[t − lag]. */
  minLag: number;
  maxLag: number;
  /** Frames at the end of `mix` used for matching. */
  window: number;
  decimation?: number;
  /** PHAT exponent: 1 = full whitening, 0 = plain correlation. */
  phat?: number;
}

export interface LagEstimate {
  lag: number;
  /** Normalized cross-correlation at `lag`, -1..1. */
  confidence: number;
  /** Least-squares gain of ref inside mix at `lag`. */
  gain: number;
}

/**
 * `ref` and `mix` are mono and index-aligned (same index = same capture frame);
 * both must hold at least `window + maxLag` frames. Returns null when the
 * inputs are too short or silent.
 */
export function estimateLag(ref: Float32Array, mix: Float32Array, options: LagOptions): LagEstimate | null {
  const n = Math.min(ref.length, mix.length);
  const d = Math.max(1, Math.floor(options.decimation ?? 8));
  const minLag = Math.max(0, Math.floor(options.minLag));
  const maxLag = Math.floor(options.maxLag);
  const window = Math.floor(options.window);
  if (maxLag < minLag || window < d * 64 || n < window + maxLag + 1) return null;

  // Coarse: x = mix tail, y = ref starting maxLag earlier. x[j] ≈ y[j + maxLag − L].
  const xStart = n - window;
  if (xStart - maxLag < 0) return null;
  const yStart = xStart - maxLag;
  const x = decimate(mix, xStart, window, d);
  const y = decimate(ref, yStart, window + maxLag, d);
  let size = 1;
  while (size < x.length + y.length) size <<= 1;
  const xr = new Float64Array(size), xi = new Float64Array(size);
  const yr = new Float64Array(size), yi = new Float64Array(size);
  const meanX = x.reduce((a, b) => a + b, 0) / x.length;
  const meanY = y.reduce((a, b) => a + b, 0) / y.length;
  for (let i = 0; i < x.length; i += 1) xr[i] = x[i] - meanX;
  for (let i = 0; i < y.length; i += 1) yr[i] = y[i] - meanY;
  fft(xr, xi);
  fft(yr, yi);
  const beta = options.phat ?? 0.8;
  let energy = 0;
  for (let k = 0; k < size; k += 1) {
    // conj(X)·Y  ->  r[m] = Σ x[j]·y[j+m]
    const re = xr[k] * yr[k] + xi[k] * yi[k];
    const im = xr[k] * yi[k] - xi[k] * yr[k];
    const mag = Math.hypot(re, im);
    energy += mag;
    const w = beta > 0 ? 1 / Math.pow(mag + 1e-12, beta) : 1;
    xr[k] = re * w;
    xi[k] = im * w;
  }
  if (energy < 1e-9) return null;
  fft(xr, xi, true);
  // m = (maxLag − L) / d, so L ∈ [minLag, maxLag] ⇔ m ∈ [0, (maxLag − minLag) / d].
  let bestM = -1;
  let best = -Infinity;
  const mMax = Math.floor((maxLag - minLag) / d);
  for (let m = 0; m <= mMax && m < size; m += 1) {
    if (xr[m] > best) { best = xr[m]; bestM = m; }
  }
  if (bestM < 0) return null;
  const coarse = maxLag - bestM * d;

  // Fine: normalized correlation at full rate over the last `fine` frames, on
  // first differences (a cheap pre-whitening). Bass-heavy music has a very
  // flat autocorrelation around zero lag, so without it the next DJ's own
  // track can pull the peak one sample off.
  const fine = Math.min(window, 48000);
  const from = Math.max(minLag, coarse - 2 * d);
  const to = Math.min(maxLag, coarse + 2 * d);
  let result: LagEstimate | null = null;
  for (let lag = from; lag <= to; lag += 1) {
    let sxy = 0, sxx = 0, syy = 0, gxy = 0, gyy = 0;
    for (let i = n - fine; i < n; i += 1) {
      const a = mix[i] - mix[i - 1];
      const b = ref[i - lag] - ref[i - lag - 1];
      sxy += a * b; sxx += a * a; syy += b * b;
      gxy += mix[i] * ref[i - lag]; gyy += ref[i - lag] * ref[i - lag];
    }
    if (sxx <= 0 || syy <= 0) continue;
    const c = sxy / Math.sqrt(sxx * syy);
    if (!result || c > result.confidence) result = { lag, confidence: c, gain: gyy > 0 ? gxy / gyy : 0 };
  }
  return result;
}
