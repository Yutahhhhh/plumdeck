//! Short-time Fourier transform: Hann-windowed frames to the half spectrum
//! (`size / 2 + 1` bins) and back. Plans and scratch are made once; the
//! per-frame calls do not allocate.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::window::hann;

pub struct Stft {
    size: usize,
    hop: usize,
    window: Vec<f32>,
    forward: Arc<dyn Fft<f32>>,
    inverse: Arc<dyn Fft<f32>>,
    buffer: Vec<Complex32>,
    scratch: Vec<Complex32>,
}

impl Stft {
    /// `size` points (any size, powers of two are fastest), frames `hop`
    /// apart.
    pub fn new(size: usize, hop: usize) -> Self {
        assert!(size >= 2 && hop >= 1 && hop <= size, "size >= 2, 1 <= hop <= size");
        let mut planner = FftPlanner::new();
        let forward = planner.plan_fft_forward(size);
        let inverse = planner.plan_fft_inverse(size);
        let scratch_len = forward.get_inplace_scratch_len().max(inverse.get_inplace_scratch_len());
        Self {
            size,
            hop,
            window: hann(size),
            forward,
            inverse,
            buffer: vec![Complex32::new(0.0, 0.0); size],
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
        }
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn hop(&self) -> usize {
        self.hop
    }

    pub fn bins(&self) -> usize {
        self.size / 2 + 1
    }

    /// The analysis window.
    pub fn window(&self) -> &[f32] {
        &self.window
    }

    /// Spectrum of one frame (`frame.len() == size`) into `spectrum`
    /// (`bins()` values).
    pub fn analyze(&mut self, frame: &[f32], spectrum: &mut [Complex32]) {
        assert!(frame.len() == self.size && spectrum.len() == self.bins());
        for ((slot, sample), weight) in self.buffer.iter_mut().zip(frame).zip(&self.window) {
            *slot = Complex32::new(sample * weight, 0.0);
        }
        self.forward.process_with_scratch(&mut self.buffer, &mut self.scratch);
        spectrum.copy_from_slice(&self.buffer[..self.bins()]);
    }

    /// The windowed frame of a half spectrum, to overlap-add `hop` apart and
    /// divide by [`Self::overlap_gain`].
    pub fn synthesize(&mut self, spectrum: &[Complex32], frame: &mut [f32]) {
        assert!(frame.len() == self.size && spectrum.len() == self.bins());
        let bins = self.bins();
        self.buffer[..bins].copy_from_slice(spectrum);
        for k in bins..self.size {
            self.buffer[k] = self.buffer[self.size - k].conj();
        }
        self.inverse.process_with_scratch(&mut self.buffer, &mut self.scratch);
        let scale = 1.0 / self.size as f32;
        for ((out, value), weight) in frame.iter_mut().zip(&self.buffer).zip(&self.window) {
            *out = value.re * scale * weight;
        }
    }

    /// `Σ w²` over the frames covering one sample: the overlap-add of
    /// analysis and synthesis windows (1.5 for Hann at `hop = size / 4`).
    pub fn overlap_gain(&self) -> f32 {
        let mut gain = 0.0f64;
        let mut offset = 0;
        while offset < self.size {
            gain += f64::from(self.window[offset]) * f64::from(self.window[offset]);
            offset += self.hop;
        }
        gain as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sine_lands_in_its_bin_and_round_trips() {
        let (size, hop) = (2048, 512);
        let mut stft = Stft::new(size, hop);
        let bin = 37;
        let signal: Vec<f32> =
            (0..size * 8).map(|n| (2.0 * std::f64::consts::PI * bin as f64 * n as f64 / size as f64).sin() as f32).collect();
        let mut spectrum = vec![Complex32::new(0.0, 0.0); stft.bins()];
        stft.analyze(&signal[..size], &mut spectrum);
        let loudest = spectrum.iter().enumerate().max_by(|a, b| a.1.norm().total_cmp(&b.1.norm())).unwrap().0;
        assert_eq!(loudest, bin);
        // Weighted overlap-add reconstructs the middle exactly.
        let mut output = vec![0.0f32; signal.len()];
        let mut frame = vec![0.0f32; size];
        let mut start = 0;
        while start + size <= signal.len() {
            stft.analyze(&signal[start..start + size], &mut spectrum);
            stft.synthesize(&spectrum, &mut frame);
            for (out, value) in output[start..start + size].iter_mut().zip(&frame) {
                *out += value;
            }
            start += hop;
        }
        let gain = stft.overlap_gain();
        assert!((gain - 1.5).abs() < 1e-6);
        for n in size..signal.len() - size {
            assert!((output[n] / gain - signal[n]).abs() < 1e-4, "sample {n}");
        }
    }
}
