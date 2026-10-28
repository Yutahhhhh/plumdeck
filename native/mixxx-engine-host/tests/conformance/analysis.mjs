// Dependency-free signal analysis for the engine conformance suite. Inputs are
// Float64Array mono signals at `rate` Hz; times are in seconds unless named Ms.

export const db = value => 20 * Math.log10(Math.max(Math.abs(value), 1e-12));

/** In-place radix-2 complex FFT. `re`/`im` lengths must be a power of two. */
export function fft(re, im, inverse = false) {
  const n = re.length;
  for (let i = 1, j = 0; i < n; ++i) {
    let bit = n >> 1;
    for (; j & bit; bit >>= 1) j ^= bit;
    j ^= bit;
    if (i < j) { [re[i], re[j]] = [re[j], re[i]]; [im[i], im[j]] = [im[j], im[i]]; }
  }
  for (let size = 2; size <= n; size <<= 1) {
    const angle = (inverse ? 2 : -2) * Math.PI / size, wr = Math.cos(angle), wi = Math.sin(angle);
    for (let start = 0; start < n; start += size) {
      let cr = 1, ci = 0;
      for (let k = 0; k < size / 2; ++k) {
        const a = start + k, b = a + size / 2;
        const tr = re[b] * cr - im[b] * ci, ti = re[b] * ci + im[b] * cr;
        re[b] = re[a] - tr; im[b] = im[a] - ti; re[a] += tr; im[a] += ti;
        const next = cr * wr - ci * wi; ci = cr * wi + ci * wr; cr = next;
      }
    }
  }
  if (inverse) for (let i = 0; i < n; ++i) { re[i] /= n; im[i] /= n; }
}

const pow2 = n => 1 << Math.ceil(Math.log2(Math.max(2, n)));

/** Cross-correlation c[lag] = sum signal[lag + k] * template[k], lag >= 0. */
export function correlate(signal, template) {
  const n = pow2(signal.length + template.length);
  const ar = new Float64Array(n), ai = new Float64Array(n), br = new Float64Array(n), bi = new Float64Array(n);
  ar.set(signal); br.set(template);
  fft(ar, ai); fft(br, bi);
  for (let i = 0; i < n; ++i) {
    const r = ar[i] * br[i] + ai[i] * bi[i], im = ai[i] * br[i] - ar[i] * bi[i];
    ar[i] = r; ai[i] = im;
  }
  fft(ar, ai, true);
  return ar.subarray(0, Math.max(0, signal.length - template.length + 1));
}

/** Parabolic peak refinement around index i of array y. */
function refine(y, i) {
  if (i <= 0 || i >= y.length - 1) return i;
  const a = y[i - 1], b = y[i], c = y[i + 1], d = a - 2 * b + c;
  return d === 0 ? i : i + 0.5 * (a - c) / d;
}

export function rms(x, from = 0, to = x.length) {
  const a = Math.max(0, Math.floor(from)), b = Math.min(x.length, Math.floor(to));
  if (b <= a) return 0;
  let sum = 0;
  for (let i = a; i < b; ++i) sum += x[i] * x[i];
  return Math.sqrt(sum / (b - a));
}

export function peak(x, from = 0, to = x.length) {
  let best = 0;
  for (let i = Math.max(0, Math.floor(from)); i < Math.min(x.length, Math.floor(to)); ++i) best = Math.max(best, Math.abs(x[i]));
  return best;
}

/** Hann-windowed single-bin amplitude of a sinusoid at `hz` (1.0 for a full-scale sine). */
export function toneAmplitude(x, rate, hz, from = 0, to = x.length) {
  const a = Math.max(0, Math.floor(from)), b = Math.min(x.length, Math.floor(to)), n = b - a;
  if (n < 16) return 0;
  let re = 0, im = 0, weight = 0;
  for (let i = 0; i < n; ++i) {
    const w = 0.5 - 0.5 * Math.cos(2 * Math.PI * i / (n - 1)), phase = 2 * Math.PI * hz * i / rate;
    re += x[a + i] * w * Math.cos(phase); im += x[a + i] * w * Math.sin(phase); weight += w;
  }
  return 2 * Math.hypot(re, im) / weight;
}

/** Strongest frequency in [lo, hi] using zero-padded Hann FFT plus parabolic refinement. */
export function dominantFrequency(x, rate, lo, hi, from = 0, to = x.length) {
  const a = Math.max(0, Math.floor(from)), b = Math.min(x.length, Math.floor(to)), n = b - a;
  const size = pow2(Math.max(n * 8, 1 << 16));
  const re = new Float64Array(size), im = new Float64Array(size);
  for (let i = 0; i < n; ++i) re[i] = x[a + i] * (0.5 - 0.5 * Math.cos(2 * Math.PI * i / (n - 1)));
  fft(re, im);
  const mag = new Float64Array(size / 2);
  for (let i = 0; i < size / 2; ++i) mag[i] = Math.log(Math.hypot(re[i], im[i]) + 1e-20);
  let best = Math.ceil(lo * size / rate);
  for (let i = best; i <= Math.floor(hi * size / rate); ++i) if (mag[i] > mag[best]) best = i;
  return refine(mag, best) * rate / size;
}

/** RMS envelope in hops of `hopMs`. */
export function envelope(x, rate, hopMs = 5) {
  const hop = Math.max(1, Math.round(rate * hopMs / 1000)), count = Math.floor(x.length / hop);
  const out = new Float64Array(count);
  for (let i = 0; i < count; ++i) out[i] = rms(x, i * hop, (i + 1) * hop);
  return out;
}

/**
 * Copies of `template` inside `signal`: correlation peaks above `floorDb`
 * relative to the strongest copy, at least `minGap` samples apart. Returns
 * sub-sample positions and gains relative to the template's own energy.
 */
export function findCopies(signal, template, { floorDb = -40, minGap = 64 } = {}) {
  const c = correlate(signal, template);
  let energy = 0;
  for (const v of template) energy += v * v;
  let strongest = 0;
  for (const v of c) strongest = Math.max(strongest, Math.abs(v));
  const magnitude = c.map(Math.abs), floor = strongest * 10 ** (floorDb / 20), found = [];
  for (let i = 1; i < c.length - 1; ++i) {
    const v = magnitude[i];
    if (v < floor || v < magnitude[i - 1] || v < magnitude[i + 1]) continue;
    const last = found.at(-1);
    if (last && i - last.index < minGap) { if (v > magnitude[last.index]) found[found.length - 1] = { index: i }; continue; }
    found.push({ index: i });
  }
  return found.map(({ index }) => ({ at: refine(magnitude, index), gain: c[index] / energy }));
}

/** Median of a numeric array. */
export function median(values) {
  if (!values.length) return NaN;
  const sorted = [...values].sort((a, b) => a - b), mid = sorted.length >> 1;
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

/**
 * Period (seconds) of the strongest repetition in [loS, hiS], from the
 * normalized autocorrelation of the signal itself (sample resolution plus
 * parabolic refinement).
 */
export function periodByAutocorrelation(x, rate, loS, hiS) {
  const n = pow2(x.length * 2), re = new Float64Array(n), im = new Float64Array(n);
  re.set(x); fft(re, im);
  for (let i = 0; i < n; ++i) { re[i] = re[i] * re[i] + im[i] * im[i]; im[i] = 0; }
  fft(re, im, true);
  const lo = Math.max(1, Math.floor(loS * rate)), hi = Math.min(x.length - 2, Math.ceil(hiS * rate));
  // Unbiased normalization keeps the first period from losing to lag 0 slope.
  const ac = new Float64Array(hi + 2);
  for (let i = 0; i <= hi + 1; ++i) ac[i] = re[i] / Math.max(1, x.length - i);
  let best = lo;
  for (let i = lo; i <= hi; ++i) if (ac[i] > ac[best]) best = i;
  return { period: refine(ac, best) / rate, strength: ac[best] / (re[0] / x.length || 1) };
}

/** Times (s) where a gate envelope rises through `level` (0..1) of its linear range. */
export function risingEdges(x, rate, { smoothMs = 1, minGapMs = 20, level = 0.5 } = {}) {
  const width = Math.max(1, Math.round(rate * smoothMs / 1000)), env = new Float64Array(x.length);
  let sum = 0;
  for (let i = 0; i < x.length; ++i) {
    sum += Math.abs(x[i]); if (i >= width) sum -= Math.abs(x[i - width]);
    env[i] = sum / Math.min(i + 1, width);
  }
  const sorted = [...env].sort((a, b) => a - b);
  const low = sorted[Math.floor(sorted.length * 0.05)], high = sorted[Math.floor(sorted.length * 0.95)], mid = low + (high - low) * level;
  const edges = [];
  for (let i = 1; i < env.length; ++i) {
    if (env[i - 1] < mid && env[i] >= mid) {
      const at = (i - 1 + (mid - env[i - 1]) / (env[i] - env[i - 1])) - width / 2;
      if (!edges.length || at - edges.at(-1) * rate > rate * minGapMs / 1000) edges.push(at / rate);
    }
  }
  return { edges, contrastDb: db(high / Math.max(low, 1e-9)) };
}

/**
 * Largest ratio (dB) of energy outside `hz ± band` to the window's total
 * energy, over sliding `windowMs` windows: clicks show up as broadband bursts.
 */
export function worstOutOfBandDb(x, rate, hz, { band = 150, windowMs = 20, from = 0, to = x.length } = {}) {
  const size = pow2(Math.round(rate * windowMs / 1000)), hop = size >> 2;
  let worst = -Infinity;
  for (let start = Math.max(0, Math.floor(from)); start + size <= Math.min(x.length, to); start += hop) {
    const re = new Float64Array(size), im = new Float64Array(size);
    // Blackman-Harris keeps the sine's own leakage far below -50 dB.
    for (let i = 0; i < size; ++i) {
      const p = 2 * Math.PI * i / (size - 1);
      re[i] = x[start + i] * (0.35875 - 0.48829 * Math.cos(p) + 0.14128 * Math.cos(2 * p) - 0.01168 * Math.cos(3 * p));
    }
    fft(re, im);
    let inside = 0, outside = 0;
    for (let k = 1; k < size / 2; ++k) {
      const e = re[k] * re[k] + im[k] * im[k], f = k * rate / size;
      if (Math.abs(f - hz) <= band) inside += e; else outside += e;
    }
    if (inside + outside > 1e-14) worst = Math.max(worst, 10 * Math.log10(Math.max(outside, 1e-20) / (inside + outside)));
  }
  return worst;
}

/**
 * Residual of `wet` against `dry` after aligning by cross-correlation (dB of
 * residual energy relative to the dry segment). `segment` is [from, to) in wet.
 */
export function residualDb(wet, dry, from, to) {
  const piece = wet.subarray(Math.floor(from), Math.floor(to));
  const c = correlate(dry, piece);
  let best = 0;
  for (let i = 1; i < c.length; ++i) if (c[i] > c[best]) best = i;
  let residual = 0, energy = 0;
  for (let i = 0; i < piece.length; ++i) {
    const ref = dry[best + i] ?? 0;
    residual += (piece[i] - ref) ** 2; energy += ref * ref;
  }
  return 10 * Math.log10(Math.max(residual, 1e-24) / Math.max(energy, 1e-24));
}

/**
 * Onsets (s) of `mine` that are not merely cross-talk from `other`: peaks of
 * the correlation with `mine` above `floorDb` of its maximum, kept only where
 * that correlation is at least twice the correlation with `other`.
 */
export function distinctOnsets(x, rate, mine, other, { floorDb = -12, minGapMs = 100 } = {}) {
  const a = correlate(x, mine), b = correlate(x, other), gap = Math.round(rate * minGapMs / 1000);
  let top = 0;
  for (const v of a) top = Math.max(top, Math.abs(v));
  const floor = top * 10 ** (floorDb / 20), found = [];
  for (let i = 1; i < a.length - 1; ++i) {
    const v = Math.abs(a[i]);
    if (v < floor || v < Math.abs(a[i - 1]) || v < Math.abs(a[i + 1])) continue;
    let cross = 0;
    for (let k = Math.max(0, i - 2); k <= Math.min(b.length - 1, i + 2); ++k) cross = Math.max(cross, Math.abs(b[k]));
    if (v < 2 * cross) continue;
    if (found.length && i - found.at(-1) < gap) { if (v > Math.abs(a[found.at(-1)])) found[found.length - 1] = i; continue; }
    found.push(i);
  }
  const magnitude = Float64Array.from(a, Math.abs);
  return found.map(i => refine(magnitude, i) / rate);
}
