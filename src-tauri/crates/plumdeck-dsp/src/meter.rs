//! Levels.

/// Root mean square of `samples` (0 for an empty slice).
pub fn rms(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum::<f64>() / samples.len() as f64).sqrt()
}

/// The largest absolute sample (0 for an empty slice).
pub fn peak(samples: &[f32]) -> f64 {
    samples.iter().fold(0.0f64, |peak, s| peak.max(f64::from(s.abs())))
}

/// `20·log10(level)`, `-inf` for silence.
pub fn dbfs(level: f64) -> f64 {
    if level > 0.0 { 20.0 * level.log10() } else { f64::NEG_INFINITY }
}

/// A running RMS with an exponential time constant (meters).
#[derive(Clone, Debug)]
pub struct RmsMeter {
    coefficient: f64,
    mean_square: f64,
}

impl RmsMeter {
    /// `time_constant` seconds at `sample_rate`.
    pub fn new(sample_rate: f64, time_constant: f64) -> Self {
        Self { coefficient: (-1.0 / (sample_rate * time_constant)).exp(), mean_square: 0.0 }
    }

    pub fn process(&mut self, samples: &[f32]) -> f64 {
        for sample in samples {
            let square = f64::from(*sample) * f64::from(*sample);
            self.mean_square = square + self.coefficient * (self.mean_square - square);
        }
        self.mean_square.sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        let sine: Vec<f32> = (0..48_000).map(|n| (2.0 * std::f64::consts::PI * 1000.0 * n as f64 / 48_000.0).sin() as f32).collect();
        assert!((rms(&sine) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((peak(&sine) - 1.0).abs() < 1e-6);
        assert!((dbfs(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(dbfs(0.0), f64::NEG_INFINITY);
        let mut meter = RmsMeter::new(48_000.0, 0.05);
        assert!((meter.process(&sine) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
        assert_eq!(rms(&[]), 0.0);
    }
}
