//! Header probing and gapless-aware audio decoding, shared by analysis,
//! waveform generation, media tools and the audio engine.
//!
//! - [`probe`]: container/codec detection and timing metadata (W1-04).
//! - [`decode`]: symphonia decoding aligned to a [`Basis`] (W1-05), streamed
//!   into [`PcmPages`], with an optional [`Resampler`].
//! - [`spec`]: the clean-room specification both follow.
//!
//! Everything here only reads the input files.

pub mod decode;
mod error;
pub mod pages;
pub mod probe;
pub mod resample;
pub mod spec;

pub use decode::{decode_mono, decode_to_pages, decode_to_pages_with, open, Basis, DecodeOptions, Decoded, Decoder, MonoPcm};
pub use error::{DecodeError, Result, Unsupported};
pub use pages::{PageWriter, PcmPages};
pub use probe::{probe_path, probe_reader, Codec, Container, PrimingSource, ProbeInfo, TimingCase};
pub use resample::{Resampler, SincResampler};

/// Identifies this decoder build in stored analysis results.
pub const DECODER_VERSION: &str = concat!("plumdeck-decode/", env!("CARGO_PKG_VERSION"), "+symphonia-0.6.1");
