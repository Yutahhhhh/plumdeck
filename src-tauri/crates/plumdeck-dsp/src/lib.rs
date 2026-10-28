//! plumdeck's own DSP building blocks (analysis.md W1-07). Shared by
//! analysis, the Rust engine and Junction, so processing never allocates:
//! every buffer is sized when a processor is built (tests/no_alloc.rs).
//!
//! - [`window`]: Hann and Kaiser windows, the Bessel I0.
//! - [`resample`]: Kaiser-windowed sinc resamplers: [`resample::Polyphase`]
//!   for a fixed rational ratio, [`resample::Variable`] for a ratio that can
//!   change per block (drift control). Pass band ≤0.1 dB up to 0.9 of the
//!   lower Nyquist, stop band ≤−80 dB from the lower Nyquist.
//! - [`stft`]: framed FFT analysis and windowed synthesis.
//! - [`biquad`]: the RBJ Audio EQ Cookbook filters (transposed direct form
//!   II, f64 state).
//! - [`meter`]: RMS, peak and dBFS.

pub mod biquad;
pub mod meter;
pub mod resample;
pub mod stft;
pub mod window;
