//! Pass band, stop band, timing and length of the resamplers.

use plumdeck_dsp::meter::{dbfs, rms};
use plumdeck_dsp::resample::{Polyphase, Variable};

fn sine(frequency: f64, rate: f64, frames: usize) -> Vec<f32> {
    (0..frames).map(|n| (2.0 * std::f64::consts::PI * frequency * n as f64 / rate).sin() as f32).collect()
}

fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    let mut resampler = Polyphase::new(1, from, to);
    let mut output = Vec::new();
    resampler.process_to_vec(input, &mut output);
    resampler.finish_to_vec(&mut output);
    output
}

/// Level of the middle of `signal` relative to a full-scale sine, in dB.
fn level(signal: &[f32]) -> f64 {
    let quarter = signal.len() / 4;
    dbfs(rms(&signal[quarter..signal.len() - quarter]) * std::f64::consts::SQRT_2)
}

#[test]
fn pass_band_is_flat_within_a_tenth_of_a_db() {
    for (from, to) in [(44_100, 48_000), (48_000, 44_100), (44_100, 16_000), (16_000, 44_100), (48_000, 96_000)] {
        let edge = 0.9 * 0.5 * f64::from(from.min(to));
        for frequency in [50.0, 1000.0, 0.5 * edge, edge] {
            let output = resample(&sine(frequency, f64::from(from), from as usize), from, to);
            let gain = level(&output);
            assert!(gain.abs() <= 0.1, "{from}->{to} at {frequency} Hz: {gain:.4} dB");
        }
    }
}

#[test]
fn stop_band_is_at_least_80_db_down() {
    for (from, to, frequency) in [(48_000, 44_100, 22_100.0), (48_000, 44_100, 23_000.0), (44_100, 16_000, 8_000.0), (96_000, 44_100, 30_000.0)] {
        let output = resample(&sine(frequency, f64::from(from), from as usize), from, to);
        let gain = level(&output);
        assert!(gain <= -80.0, "{from}->{to} at {frequency} Hz: {gain:.1} dB");
    }
}

#[test]
fn output_matches_the_ideal_samples_at_the_scaled_times() {
    let (from, to) = (16_000u32, 44_100u32);
    let output = resample(&sine(1000.0, f64::from(from), 16_000), from, to);
    let ideal = sine(1000.0, f64::from(to), output.len());
    let middle = output.len() / 4..output.len() * 3 / 4;
    let error: Vec<f32> = middle.clone().map(|n| output[n] - ideal[n]).collect();
    assert!(dbfs(rms(&error)) < -80.0, "error {:.1} dB", dbfs(rms(&error)));
}

#[test]
fn frame_zero_stays_frame_zero_and_lengths_round_up() {
    let mut impulse = vec![0.0f32; 1000];
    impulse[100] = 1.0;
    let output = resample(&impulse, 44_100, 48_000);
    assert_eq!(output.len(), (1000u64 * 48_000).div_ceil(44_100) as usize);
    let peak = output.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
    assert_eq!(peak, (100.0f64 * 48_000.0 / 44_100.0).round() as usize);
    for frames in [0usize, 1, 7, 441, 4410] {
        let output = resample(&vec![0.25; frames], 44_100, 48_000);
        assert_eq!(output.len() as u64, (frames as u64 * 48_000).div_ceil(44_100), "{frames} frames");
    }
}

#[test]
fn stereo_channels_stay_separate() {
    let left = sine(440.0, 48_000.0, 4800);
    let interleaved: Vec<f32> = left.iter().flat_map(|l| [*l, 0.0]).collect();
    let mut resampler = Polyphase::new(2, 48_000, 44_100);
    let mut output = Vec::new();
    resampler.process_to_vec(&interleaved, &mut output);
    resampler.finish_to_vec(&mut output);
    assert!(output.chunks(2).all(|frame| frame[1] == 0.0));
    assert!(output.chunks(2).any(|frame| frame[0].abs() > 0.5));
}

#[test]
fn unusual_ratios_use_the_table_and_keep_the_length() {
    let input = sine(1000.0, 44_100.0, 44_100);
    let output = resample(&input, 44_100, 44_099);
    assert_eq!(output.len() as u64, (44_100u64 * 44_099).div_ceil(44_100));
    assert!(level(&output).abs() <= 0.1);
}

#[test]
fn variable_ratio_tracks_a_drifting_clock() {
    let input = sine(1000.0, 48_000.0, 48_000);
    let mut resampler = Variable::new(1, 1.0, 0.01);
    let mut output = vec![0.0f32; 60_000];
    let mut produced = 0;
    let mut consumed = 0;
    for block in 0..100 {
        resampler.set_ratio(if block % 2 == 0 { 1.001 } else { 0.999 });
        let end = (consumed + 480).min(input.len());
        let done = resampler.process(&input[consumed..end], &mut output[produced..]);
        consumed += done.consumed;
        produced += done.produced;
    }
    assert_eq!(consumed, input.len());
    assert!((produced as i64 - 48_000).abs() < 200, "{produced}");
    assert!(level(&output[..produced]).abs() <= 0.1);
}
