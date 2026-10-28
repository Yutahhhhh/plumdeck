//! Second-order sections from Robert Bristow-Johnson's "Audio EQ Cookbook"
//! (public). Coefficients are normalized by `a0`; processing is the
//! transposed direct form II with f64 state, one channel per [`Biquad`].

use std::f64::consts::PI;

/// The cookbook filter shapes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Lowpass,
    Highpass,
    /// Constant 0 dB peak gain.
    Bandpass,
    Notch,
    Allpass,
    /// Gain in dB at the center.
    Peaking { gain_db: f64 },
    /// Shelf gain in dB; `q` is the shelf's Q.
    LowShelf { gain_db: f64 },
    HighShelf { gain_db: f64 },
}

/// Normalized coefficients `b0 b1 b2 a1 a2`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefficients {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a1: f64,
    pub a2: f64,
}

impl Coefficients {
    /// The cookbook design for `shape` at `frequency` Hz, `q`, `sample_rate`.
    pub fn design(shape: Shape, frequency: f64, q: f64, sample_rate: f64) -> Self {
        let w0 = 2.0 * PI * frequency / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Lowpass => ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Shape::Highpass => ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Shape::Bandpass => (alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Shape::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Shape::Allpass => (1.0 - alpha, -2.0 * cos, 1.0 + alpha, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            Shape::Peaking { gain_db } => {
                let a = 10f64.powf(gain_db / 40.0);
                (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
            }
            Shape::LowShelf { gain_db } => {
                let a = 10f64.powf(gain_db / 40.0);
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            Shape::HighShelf { gain_db } => {
                let a = 10f64.powf(gain_db / 40.0);
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// `|H(e^{jω})|` at `frequency` Hz.
    pub fn magnitude(&self, frequency: f64, sample_rate: f64) -> f64 {
        let w = 2.0 * PI * frequency / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let num = ((self.b0 + self.b1 * c1 + self.b2 * c2).powi(2) + (self.b1 * s1 + self.b2 * s2).powi(2)).sqrt();
        let den = ((1.0 + self.a1 * c1 + self.a2 * c2).powi(2) + (self.a1 * s1 + self.a2 * s2).powi(2)).sqrt();
        num / den
    }
}

/// One channel's filter state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    pub coefficients: Coefficients,
    z1: f64,
    z2: f64,
}

impl Biquad {
    pub fn new(coefficients: Coefficients) -> Self {
        Self { coefficients, z1: 0.0, z2: 0.0 }
    }

    /// Change the coefficients and keep the state (parameter sweeps).
    pub fn set(&mut self, coefficients: Coefficients) {
        self.coefficients = coefficients;
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    #[inline]
    pub fn tick(&mut self, input: f32) -> f32 {
        let c = &self.coefficients;
        let x = f64::from(input);
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        y as f32
    }

    /// Filter `samples` in place.
    pub fn process(&mut self, samples: &mut [f32]) {
        for sample in samples {
            *sample = self.tick(*sample);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meter::{dbfs, rms};

    const FS: f64 = 48_000.0;

    fn db(c: &Coefficients, frequency: f64) -> f64 {
        20.0 * c.magnitude(frequency, FS).log10()
    }

    #[test]
    fn cookbook_responses() {
        let q = std::f64::consts::FRAC_1_SQRT_2;
        let lowpass = Coefficients::design(Shape::Lowpass, 1000.0, q, FS);
        assert!((db(&lowpass, 1000.0) + 3.0103).abs() < 1e-3);
        assert!(db(&lowpass, 10.0).abs() < 1e-3);
        assert!(db(&lowpass, 10_000.0) < -38.0);
        let highpass = Coefficients::design(Shape::Highpass, 1000.0, q, FS);
        assert!((db(&highpass, 1000.0) + 3.0103).abs() < 1e-3);
        let peak = Coefficients::design(Shape::Peaking { gain_db: 6.0 }, 2000.0, 1.0, FS);
        assert!((db(&peak, 2000.0) - 6.0).abs() < 1e-9);
        assert!(db(&peak, 20.0).abs() < 0.01);
        let notch = Coefficients::design(Shape::Notch, 3000.0, 2.0, FS);
        assert!(db(&notch, 3000.0) < -100.0);
        let allpass = Coefficients::design(Shape::Allpass, 500.0, 0.7, FS);
        for f in [50.0, 500.0, 5000.0] {
            assert!(db(&allpass, f).abs() < 1e-9);
        }
        let low = Coefficients::design(Shape::LowShelf { gain_db: -12.0 }, 200.0, q, FS);
        assert!((db(&low, 10.0) + 12.0).abs() < 0.05 && db(&low, 10_000.0).abs() < 0.05);
        assert!((db(&low, 200.0) + 6.0).abs() < 0.05, "half the shelf gain at the corner");
        let high = Coefficients::design(Shape::HighShelf { gain_db: 9.0 }, 4000.0, q, FS);
        assert!((db(&high, 20_000.0) - 9.0).abs() < 0.2 && db(&high, 20.0).abs() < 0.05);
        let band = Coefficients::design(Shape::Bandpass, 1000.0, 2.0, FS);
        assert!(db(&band, 1000.0).abs() < 1e-9);
    }

    #[test]
    fn filtering_matches_the_response() {
        let coefficients = Coefficients::design(Shape::Lowpass, 2000.0, std::f64::consts::FRAC_1_SQRT_2, FS);
        let mut filter = Biquad::new(coefficients);
        let mut tone: Vec<f32> = (0..48_000).map(|n| (2.0 * PI * 4000.0 * n as f64 / FS).sin() as f32).collect();
        filter.process(&mut tone);
        let measured = dbfs(rms(&tone[4800..]) * std::f64::consts::SQRT_2);
        assert!((measured - db(&coefficients, 4000.0)).abs() < 0.05, "{measured}");
        filter.reset();
        assert_eq!(filter.tick(0.0), 0.0);
    }
}
