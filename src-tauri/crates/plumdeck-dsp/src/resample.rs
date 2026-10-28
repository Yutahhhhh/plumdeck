//! Kaiser-windowed sinc resamplers over interleaved `f32` frames.
//!
//! Both keep `plumdeck-decode`'s timing contract: output frame `k` is the
//! input signal at input time `k · in_rate / out_rate` (frame 0 stays frame
//! 0, positions scale exactly with the rate); samples before the start count
//! as silence; after [`Polyphase::finish`] the output has
//! `ceil(input_frames · out_rate / in_rate)` frames.
//!
//! Filter: a lowpass at 0.95 of the lower Nyquist with a Kaiser window for
//! 90 dB (β ≈ 8.96) over ±64 zero crossings of the lower rate. Pass band
//! (to 0.9 of the lower Nyquist) is flat within 0.1 dB and everything from
//! the lower Nyquist up is at least 80 dB down. Each phase is normalized to
//! an exact DC gain of 1.
//!
//! Processing is allocation-free: input is copied into a history buffer
//! sized at construction, as much as fits per call, and output goes to a
//! caller slice. [`Process`] says how much of each was used. No input may
//! follow `finish`.

use crate::window::{kaiser, kaiser_beta, sinc};

/// Zero crossings on each side, counted at the lower of the two rates.
const ZERO_CROSSINGS: f64 = 64.0;
/// Cutoff as a fraction of the lower Nyquist.
const CUTOFF: f64 = 0.95;
/// Stop-band design target (dB).
const ATTENUATION_DB: f64 = 90.0;
/// Frames of input accepted per refill beyond the filter span.
const BLOCK_FRAMES: usize = 1024;
/// Above this many phases a rational ratio uses the interpolated table.
const MAX_PHASES: u64 = 2048;
/// Table rows per input sample in [`Variable`].
const TABLE_RESOLUTION: usize = 512;

/// How much one call consumed and produced, in frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Process {
    pub consumed: usize,
    pub produced: usize,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The continuous kernel for a ratio `out/in` (input-sample time `t`).
struct Kernel {
    /// Cutoff in cycles per input sample.
    cutoff: f64,
    /// Half width in input samples.
    half: f64,
    beta: f64,
}

impl Kernel {
    fn new(ratio: f64) -> Self {
        let scale = ratio.min(1.0);
        Self { cutoff: 0.5 * CUTOFF * scale, half: ZERO_CROSSINGS / scale, beta: kaiser_beta(ATTENUATION_DB) }
    }

    fn at(&self, t: f64) -> f64 {
        2.0 * self.cutoff * sinc(2.0 * self.cutoff * t) * kaiser(t, self.half, self.beta)
    }

    /// Taps on each side (input samples).
    fn taps_per_side(&self) -> usize {
        self.half.ceil() as usize
    }

    /// One phase: coefficients for input samples `n − side + 1 ..= n + side`
    /// at fractional position `frac` past `n`, DC-normalized.
    fn row(&self, frac: f64, side: usize, out: &mut [f32]) {
        let mut sum = 0.0;
        for (j, slot) in out.iter_mut().enumerate() {
            let value = self.at(frac + side as f64 - 1.0 - j as f64);
            sum += value;
            *slot = value as f32;
        }
        if sum != 0.0 {
            for slot in out.iter_mut() {
                *slot = (f64::from(*slot) / sum) as f32;
            }
        }
    }
}

/// History of interleaved input with a fixed capacity, starting at absolute
/// input frame `start` (negative: the virtual silence before frame 0).
struct History {
    channels: usize,
    data: Vec<f32>,
    frames: usize,
    capacity: usize,
    start: i64,
}

impl History {
    fn new(channels: usize, side: usize) -> Self {
        let capacity = 2 * side + BLOCK_FRAMES;
        // `side − 1` frames of leading silence so frame 0 has full support.
        let lead = side.saturating_sub(1);
        let data = vec![0.0; capacity * channels];
        Self { channels, data, frames: lead, capacity, start: -(lead as i64) }
    }

    fn end(&self) -> i64 {
        self.start + self.frames as i64
    }

    /// Copy as many frames of `input` as fit; returns frames taken.
    fn fill(&mut self, input: &[f32]) -> usize {
        let room = self.capacity - self.frames;
        let take = room.min(input.len() / self.channels);
        let from = self.frames * self.channels;
        self.data[from..from + take * self.channels].copy_from_slice(&input[..take * self.channels]);
        self.frames += take;
        take
    }

    fn fill_silence(&mut self, frames: usize) -> usize {
        let take = (self.capacity - self.frames).min(frames);
        let from = self.frames * self.channels;
        self.data[from..from + take * self.channels].fill(0.0);
        self.frames += take;
        take
    }

    /// Drop frames before absolute frame `keep_from`.
    fn discard_before(&mut self, keep_from: i64) {
        let drop = (keep_from - self.start).clamp(0, self.frames as i64) as usize;
        if drop == 0 {
            return;
        }
        self.data.copy_within(drop * self.channels..self.frames * self.channels, 0);
        self.frames -= drop;
        self.start += drop as i64;
    }

    /// Dot product of `row` with the frames from absolute `first`, one channel.
    fn dot(&self, first: i64, row: &[f32], channel: usize) -> f32 {
        let offset = (first - self.start) as usize;
        let mut acc = 0.0f32;
        for (j, coefficient) in row.iter().enumerate() {
            acc += coefficient * self.data[(offset + j) * self.channels + channel];
        }
        acc
    }
}

/// A resampler for a fixed rational ratio, one coefficient row per phase.
/// Ratios with more than 2048 phases (unusual rates) use [`Variable`]'s
/// interpolated table through [`Polyphase::new`] transparently.
pub struct Polyphase {
    channels: usize,
    up: u64,
    down: u64,
    side: usize,
    rows: Vec<f32>,
    /// `Some` when the ratio has too many phases for exact rows.
    table: Option<Variable>,
    history: History,
    /// Next output frame.
    next: u64,
    input_frames: u64,
    finished: bool,
}

impl Polyphase {
    pub fn new(channels: usize, input_rate: u32, output_rate: u32) -> Self {
        assert!(channels > 0 && input_rate > 0 && output_rate > 0, "positive channels and rates");
        let g = gcd(u64::from(input_rate), u64::from(output_rate));
        let (up, down) = (u64::from(output_rate) / g, u64::from(input_rate) / g);
        let kernel = Kernel::new(up as f64 / down as f64);
        let side = kernel.taps_per_side();
        let (rows, table) = if up <= MAX_PHASES {
            let taps = 2 * side;
            let mut rows = vec![0.0f32; up as usize * taps];
            for phase in 0..up as usize {
                kernel.row(phase as f64 / up as f64, side, &mut rows[phase * taps..(phase + 1) * taps]);
            }
            (rows, None)
        } else {
            (Vec::new(), Some(Variable::new(channels, f64::from(output_rate) / f64::from(input_rate), 0.0)))
        };
        Self { channels, up, down, side, rows, table, history: History::new(channels, side), next: 0, input_frames: 0, finished: false }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Output frames produced so far.
    pub fn produced(&self) -> u64 {
        self.next
    }

    fn total_after_finish(&self) -> u64 {
        (self.input_frames * self.up).div_ceil(self.down)
    }

    fn emit(&mut self, output: &mut [f32]) -> usize {
        let taps = 2 * self.side;
        let mut produced = 0;
        while (produced + 1) * self.channels <= output.len() {
            if self.finished && self.next >= self.total_after_finish() {
                break;
            }
            let numerator = self.next * self.down;
            let n = (numerator / self.up) as i64;
            let phase = (numerator % self.up) as usize;
            let first = n - self.side as i64 + 1;
            if n + self.side as i64 >= self.history.end() {
                break;
            }
            let row = &self.rows[phase * taps..(phase + 1) * taps];
            for channel in 0..self.channels {
                output[produced * self.channels + channel] = self.history.dot(first, row, channel);
            }
            produced += 1;
            self.next += 1;
        }
        let keep_from = ((self.next * self.down) / self.up) as i64 - self.side as i64 + 1;
        self.history.discard_before(keep_from);
        produced
    }

    /// Resample `input` (interleaved) into `output`; never allocates. Call
    /// again with the unconsumed rest when `consumed` is short.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Process {
        if let Some(table) = &mut self.table {
            let done = table.process(input, output);
            self.input_frames += done.consumed as u64;
            self.next += done.produced as u64;
            return done;
        }
        let mut done = Process::default();
        loop {
            let taken = self.history.fill(&input[done.consumed * self.channels..]);
            done.consumed += taken;
            self.input_frames += taken as u64;
            let produced = self.emit(&mut output[done.produced * self.channels..]);
            done.produced += produced;
            if (taken == 0 && produced == 0) || done.consumed * self.channels >= input.len() && produced == 0 {
                break;
            }
        }
        done
    }

    /// Flush the tail into `output`; call until it returns 0.
    pub fn finish(&mut self, output: &mut [f32]) -> usize {
        let target = self.total_after_finish();
        if let Some(table) = &mut self.table {
            let produced = table.finish_to(target - self.next.min(target), output);
            self.next += produced as u64;
            return produced;
        }
        self.finished = true;
        let mut produced = 0;
        loop {
            self.history.fill_silence(self.side + 1);
            let step = self.emit(&mut output[produced * self.channels..]);
            produced += step;
            if step == 0 {
                break;
            }
        }
        produced
    }

    /// [`Self::process`] into a growing Vec (allocates; for offline use).
    pub fn process_to_vec(&mut self, mut input: &[f32], output: &mut Vec<f32>) {
        let mut chunk = vec![0.0f32; 4096 * self.channels];
        loop {
            let done = self.process(input, &mut chunk);
            output.extend_from_slice(&chunk[..done.produced * self.channels]);
            input = &input[done.consumed * self.channels..];
            if done.consumed == 0 && done.produced == 0 {
                break;
            }
        }
    }

    /// [`Self::finish`] into a growing Vec.
    pub fn finish_to_vec(&mut self, output: &mut Vec<f32>) {
        let mut chunk = vec![0.0f32; 4096 * self.channels];
        loop {
            let produced = self.finish(&mut chunk);
            if produced == 0 {
                break;
            }
            output.extend_from_slice(&chunk[..produced * self.channels]);
        }
    }
}

/// A resampler whose ratio (`out/in`) may change between calls, for clock
/// drift control (microphone, Junction input). Coefficients come from a
/// table of 512 rows per input sample, linearly interpolated. The filter is
/// designed for the lowest ratio given at construction.
pub struct Variable {
    channels: usize,
    side: usize,
    table: Vec<f32>,
    history: History,
    /// Input time of the next output, in absolute input frames.
    position: f64,
    step: f64,
    row: Vec<f32>,
    produced_total: u64,
}

impl Variable {
    /// `ratio` is out/in; the filter allows the ratio to drop by
    /// `max_deviation` (fraction, e.g. 0.01) without aliasing.
    pub fn new(channels: usize, ratio: f64, max_deviation: f64) -> Self {
        assert!(channels > 0 && ratio > 0.0 && ratio.is_finite(), "positive channels and ratio");
        let kernel = Kernel::new(ratio * (1.0 - max_deviation.clamp(0.0, 0.5)));
        let side = kernel.taps_per_side();
        let taps = 2 * side;
        let mut table = vec![0.0f32; (TABLE_RESOLUTION + 1) * taps];
        for row in 0..=TABLE_RESOLUTION {
            kernel.row(row as f64 / TABLE_RESOLUTION as f64, side, &mut table[row * taps..(row + 1) * taps]);
        }
        Self {
            channels,
            side,
            table,
            history: History::new(channels, side),
            position: 0.0,
            step: 1.0 / ratio,
            row: vec![0.0; taps],
            produced_total: 0,
        }
    }

    /// Change the ratio (out/in) for the following outputs.
    pub fn set_ratio(&mut self, ratio: f64) {
        if ratio > 0.0 && ratio.is_finite() {
            self.step = 1.0 / ratio;
        }
    }

    fn emit(&mut self, output: &mut [f32], limit: Option<u64>) -> usize {
        let taps = 2 * self.side;
        let mut produced = 0;
        while (produced + 1) * self.channels <= output.len() {
            if limit.is_some_and(|limit| produced as u64 >= limit) {
                break;
            }
            let n = self.position.floor() as i64;
            if n + self.side as i64 >= self.history.end() {
                break;
            }
            let scaled = (self.position - n as f64) * TABLE_RESOLUTION as f64;
            let lower = (scaled.floor() as usize).min(TABLE_RESOLUTION - 1);
            let mix = (scaled - lower as f64) as f32;
            let (a, b) = (&self.table[lower * taps..(lower + 1) * taps], &self.table[(lower + 1) * taps..(lower + 2) * taps]);
            for (slot, (x, y)) in self.row.iter_mut().zip(a.iter().zip(b)) {
                *slot = x + (y - x) * mix;
            }
            let first = n - self.side as i64 + 1;
            for channel in 0..self.channels {
                output[produced * self.channels + channel] = self.history.dot(first, &self.row, channel);
            }
            produced += 1;
            self.position += self.step;
        }
        self.history.discard_before(self.position.floor() as i64 - self.side as i64 + 1);
        self.produced_total += produced as u64;
        produced
    }

    /// Resample `input` into `output`; never allocates.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Process {
        let mut done = Process::default();
        loop {
            let taken = self.history.fill(&input[done.consumed * self.channels..]);
            done.consumed += taken;
            let produced = self.emit(&mut output[done.produced * self.channels..], None);
            done.produced += produced;
            if (taken == 0 && produced == 0) || done.consumed * self.channels >= input.len() && produced == 0 {
                break;
            }
        }
        done
    }

    /// Flush up to `frames` more outputs from the tail (silence after the
    /// input); returns the frames written.
    pub fn finish_to(&mut self, frames: u64, output: &mut [f32]) -> usize {
        let mut produced = 0usize;
        loop {
            self.history.fill_silence(self.side + 1);
            let step = self.emit(&mut output[produced * self.channels..], Some(frames - produced as u64));
            produced += step;
            if step == 0 {
                break;
            }
        }
        produced
    }
}
