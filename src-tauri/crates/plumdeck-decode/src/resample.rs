//! Resampler hook and a small built-in windowed-sinc resampler.
//!
//! The production resampler lives in `plumdeck-dsp` (W1-07) and plugs in
//! through [`Resampler`]. The built-in [`SincResampler`] is written for
//! analysis: a Kaiser-windowed sinc evaluated from a linearly interpolated
//! table, with per-output normalisation for exact DC gain. It is
//! zero-latency: output frame `k` is the input signal evaluated at input time
//! `k * in_rate / out_rate`, so frame 0 stays frame 0 and sample positions
//! scale exactly with the rate.

/// Streaming resampler over interleaved `f32` frames.
pub trait Resampler: Send {
    fn channels(&self) -> usize;
    fn input_rate(&self) -> u32;
    fn output_rate(&self) -> u32;
    /// Consumes interleaved input and appends resampled interleaved output.
    fn process(&mut self, input: &[f32], output: &mut Vec<f32>);
    /// Flushes the tail. After this the total output is
    /// `ceil(input_frames * output_rate / input_rate)` frames.
    fn finish(&mut self, output: &mut Vec<f32>);
}

/// Builds a resampler for `(channels, input_rate, output_rate)`.
pub type ResamplerFactory = dyn Fn(usize, u32, u32) -> Box<dyn Resampler>;

/// The default factory: [`SincResampler`].
pub fn default_resampler(channels: usize, input_rate: u32, output_rate: u32) -> Box<dyn Resampler> {
    Box::new(SincResampler::new(channels, input_rate, output_rate))
}

const ZERO_CROSSINGS: f64 = 16.0;
const TABLE_RES: usize = 256;
const KAISER_BETA: f64 = 8.0;
const ROLLOFF: f64 = 0.94;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Windowed-sinc resampler (see the module docs).
#[derive(Debug, Clone)]
pub struct SincResampler {
    channels: usize,
    in_rate: u32,
    out_rate: u32,
    /// Input step per output frame as the fraction `num / den`.
    num: u64,
    den: u64,
    /// Kernel half-width in input samples.
    half_width: f64,
    /// One-sided kernel table, `TABLE_RES` entries per input sample.
    table: Vec<f64>,
    /// Buffered input (interleaved) starting at absolute frame `buf_start`.
    buf: Vec<f32>,
    buf_start: u64,
    in_frames: u64,
    /// Next output frame index.
    next_out: u64,
    weights: Vec<f64>,
}

impl SincResampler {
    pub fn new(channels: usize, in_rate: u32, out_rate: u32) -> Self {
        let channels = channels.max(1);
        let (in_rate, out_rate) = (in_rate.max(1), out_rate.max(1));
        let g = gcd(u64::from(in_rate), u64::from(out_rate));
        let (num, den) = (u64::from(in_rate) / g, u64::from(out_rate) / g);
        // Cutoff as a fraction of the input sample rate (Nyquist = 0.5).
        // The ratio floor bounds the kernel table (about 280k entries at
        // most); stronger decimation than 64:1 aliases instead.
        let cutoff = 0.5 * ROLLOFF * (f64::from(out_rate) / f64::from(in_rate)).clamp(1.0 / 64.0, 1.0);
        let half_width = ZERO_CROSSINGS / (2.0 * cutoff);
        let len = (half_width * TABLE_RES as f64).ceil() as usize + 2;
        let i0_beta = bessel_i0(KAISER_BETA);
        let table = (0..len)
            .map(|i| {
                let x = i as f64 / TABLE_RES as f64;
                if x >= half_width {
                    return 0.0;
                }
                let arg = 2.0 * cutoff * x;
                let sinc = if arg == 0.0 { 1.0 } else { (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg) };
                let r = x / half_width;
                let window = bessel_i0(KAISER_BETA * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
                sinc * window
            })
            .collect();
        Self {
            channels,
            in_rate,
            out_rate,
            num,
            den,
            half_width,
            table,
            buf: Vec::new(),
            buf_start: 0,
            in_frames: 0,
            next_out: 0,
            weights: Vec::new(),
        }
    }

    fn kernel(&self, distance: f64) -> f64 {
        let pos = distance.abs() * TABLE_RES as f64;
        let i = pos as usize;
        if i + 1 >= self.table.len() {
            return 0.0;
        }
        let f = pos - i as f64;
        self.table[i] * (1.0 - f) + self.table[i + 1] * f
    }

    /// Produces outputs whose kernel support ends before `limit` (absolute
    /// input frame, exclusive); input beyond the data is zero.
    fn run(&mut self, limit: u64, max_out: u64, output: &mut Vec<f32>) {
        let ch = self.channels;
        let hw = self.half_width;
        while self.next_out < max_out {
            let pos_num = u128::from(self.next_out) * u128::from(self.num);
            let ipos = (pos_num / u128::from(self.den)) as u64;
            let frac = (pos_num % u128::from(self.den)) as f64 / self.den as f64;
            let t = ipos as f64 + frac;
            let hi = (t + hw).floor() as u64;
            if hi >= limit && limit != u64::MAX {
                break;
            }
            let lo = (t - hw).ceil().max(0.0) as u64;
            // Weights for every tap including virtual zeros before frame 0.
            let lo_virtual = (t - hw).ceil() as i64;
            self.weights.clear();
            let mut sum = 0.0;
            for i in lo_virtual..=hi as i64 {
                let w = self.kernel(t - i as f64);
                sum += w;
                if i >= 0 {
                    self.weights.push(w);
                }
            }
            let norm = if sum.abs() > 1e-12 { 1.0 / sum } else { 0.0 };
            let base = output.len();
            output.resize(base + ch, 0.0);
            for (k, &w) in self.weights.iter().enumerate() {
                let frame = lo + k as u64;
                if frame >= self.in_frames || frame < self.buf_start {
                    continue;
                }
                let at = ((frame - self.buf_start) as usize) * ch;
                for c in 0..ch {
                    output[base + c] += (f64::from(self.buf[at + c]) * w * norm) as f32;
                }
            }
            self.next_out += 1;
        }
        // Drop input no longer needed by the next output.
        let pos_num = u128::from(self.next_out) * u128::from(self.num);
        let t_next = (pos_num / u128::from(self.den)) as f64;
        let keep_from = ((t_next - hw).floor().max(0.0) as u64).saturating_sub(1).min(self.in_frames);
        if keep_from > self.buf_start {
            let drop = ((keep_from - self.buf_start) as usize * ch).min(self.buf.len());
            self.buf.drain(..drop);
            self.buf_start = keep_from;
        }
    }

    fn total_out(&self) -> u64 {
        // ceil(in_frames * den / num)
        let n = u128::from(self.in_frames) * u128::from(self.den);
        n.div_ceil(u128::from(self.num)) as u64
    }
}

impl Resampler for SincResampler {
    fn channels(&self) -> usize {
        self.channels
    }

    fn input_rate(&self) -> u32 {
        self.in_rate
    }

    fn output_rate(&self) -> u32 {
        self.out_rate
    }

    fn process(&mut self, input: &[f32], output: &mut Vec<f32>) {
        let ch = self.channels;
        let input = &input[..input.len() - input.len() % ch];
        if self.num == self.den {
            output.extend_from_slice(input);
            self.in_frames += (input.len() / ch) as u64;
            self.next_out = self.in_frames;
            return;
        }
        self.buf.extend_from_slice(input);
        self.in_frames += (input.len() / ch) as u64;
        let limit = self.in_frames;
        self.run(limit, u64::MAX, output);
    }

    fn finish(&mut self, output: &mut Vec<f32>) {
        if self.num == self.den {
            return;
        }
        let total = self.total_out();
        self.run(u64::MAX, total, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, n: usize, phase_offset: f64) -> Vec<f32> {
        (0..n).map(|i| ((i as f64 + phase_offset) / f64::from(rate) * freq * std::f64::consts::TAU).sin() as f32).collect()
    }

    fn check(in_rate: u32, out_rate: u32, chunk: usize) {
        let n = in_rate as usize; // 1 s
        let input = sine(in_rate, 1000.0, n, 0.0);
        let mut r = SincResampler::new(1, in_rate, out_rate);
        let mut out = Vec::new();
        for c in input.chunks(chunk) {
            r.process(c, &mut out);
        }
        r.finish(&mut out);
        let expect_len = (n as u64 * u64::from(out_rate)).div_ceil(u64::from(in_rate)) as usize;
        assert_eq!(out.len(), expect_len, "{in_rate}->{out_rate}");
        let reference = sine(out_rate, 1000.0, out.len(), 0.0);
        let margin = out_rate as usize / 20;
        let err = out[margin..out.len() - margin]
            .iter()
            .zip(&reference[margin..])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 2e-3, "{in_rate}->{out_rate} max err {err}");
    }

    #[test]
    fn sine_accuracy_and_length() {
        check(44_100, 22_050, 1000);
        check(48_000, 44_100, 777);
        check(44_100, 48_000, 4096);
        check(22_050, 44_100, 333);
        check(44_100, 44_100, 100);
    }

    #[test]
    fn impulse_stays_aligned() {
        let mut input = vec![0.0f32; 44_100];
        input[20_000] = 1.0;
        let mut r = SincResampler::new(1, 44_100, 22_050);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.finish(&mut out);
        let peak = out.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i);
        assert_eq!(peak, Some(10_000));
    }

    #[test]
    fn extreme_ratio_has_bounded_table() {
        let r = SincResampler::new(1, 768_000, 1_000);
        assert!(r.table.len() < 300_000, "{}", r.table.len());
        let mut r = r;
        let mut out = Vec::new();
        r.process(&vec![0.5; 76_800], &mut out);
        r.finish(&mut out);
        assert_eq!(out.len(), 100);
    }

    #[test]
    fn rejects_aliases() {
        // 15 kHz at 44.1 kHz must vanish when going to 22.05 kHz.
        let input = sine(44_100, 15_000.0, 44_100, 0.0);
        let mut r = SincResampler::new(1, 44_100, 22_050);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        r.finish(&mut out);
        let rms = (out[2000..20_000].iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / 18_000.0).sqrt();
        assert!(rms < 1e-3, "alias rms {rms}");
    }
}
