//! Windows.

use std::f64::consts::PI;

/// The modified Bessel function of the first kind, order 0 (power series).
pub fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// The Kaiser window at `t` for a half width `half`: `I0(β·√(1−(t/half)²)) / I0(β)`,
/// zero outside `|t| < half`.
pub fn kaiser(t: f64, half: f64, beta: f64) -> f64 {
    let ratio = t / half;
    if ratio.abs() >= 1.0 {
        return 0.0;
    }
    bessel_i0(beta * (1.0 - ratio * ratio).sqrt()) / bessel_i0(beta)
}

/// Kaiser's β for a stop-band attenuation of `attenuation_db` (> 50 dB).
pub fn kaiser_beta(attenuation_db: f64) -> f64 {
    0.1102 * (attenuation_db - 8.7)
}

/// A periodic Hann window of `size` points (sums to `size / 2`; overlap-adds
/// to a constant at hops of `size / 2` and `size / 4`).
pub fn hann(size: usize) -> Vec<f32> {
    (0..size).map(|n| (0.5 - 0.5 * (2.0 * PI * n as f64 / size as f64).cos()) as f32).collect()
}

/// `sin(πx) / (πx)`.
pub fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_have_their_textbook_values() {
        assert!((bessel_i0(0.0) - 1.0).abs() < 1e-15);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_008_4).abs() < 1e-14);
        assert!((kaiser(0.0, 10.0, 9.0) - 1.0).abs() < 1e-15);
        assert_eq!(kaiser(10.0, 10.0, 9.0), 0.0);
        let window = hann(8);
        assert_eq!(window[0], 0.0);
        assert!((window[4] - 1.0).abs() < 1e-7);
        assert!((window.iter().map(|w| f64::from(*w)).sum::<f64>() - 4.0).abs() < 1e-6);
        assert_eq!(sinc(0.0), 1.0);
        assert!(sinc(1.0).abs() < 1e-15);
    }
}
