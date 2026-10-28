//! Header probing (W1-04): container and codec detection, stream parameters
//! and the timing metadata described in [`crate::spec`].

mod caf;
mod flac;
mod io;
mod mp4;
pub(crate) mod mpeg;
mod ogg;
mod riff;

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use crate::error::Result;

pub use mp4::Mp4Details;
pub use mpeg::{InfoTag, LameTag, Mp3Details};

/// Container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// Raw MPEG audio elementary stream (`.mp3`, `.mp2`).
    MpegAudio,
    Mp4,
    Adts,
    Flac,
    Wav,
    Aiff,
    Caf,
    Ogg,
    Unknown,
}

impl Container {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MpegAudio => "mpeg-audio",
            Self::Mp4 => "mp4",
            Self::Adts => "adts",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Aiff => "aiff",
            Self::Caf => "caf",
            Self::Ogg => "ogg",
            Self::Unknown => "unknown",
        }
    }
}

/// Codec of the probed audio stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    Mp1,
    Mp2,
    Mp3,
    AacLc,
    /// HE-AAC v1 (SBR) or v2 (`ps`). `implicit`: see spec 4.2.
    HeAac { ps: bool, implicit: bool },
    /// Another MPEG-4 audio object type.
    AacOther(u8),
    Alac,
    Flac,
    /// Integer PCM.
    Pcm,
    /// IEEE float PCM.
    PcmFloat,
    /// Compressed AIFF-C or WAV formats other than PCM/float.
    PcmOther,
    Vorbis,
    Opus,
    Unknown,
}

impl Codec {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp1 => "mp1",
            Self::Mp2 => "mp2",
            Self::Mp3 => "mp3",
            Self::AacLc => "aac-lc",
            Self::HeAac { ps: false, .. } => "he-aac",
            Self::HeAac { ps: true, .. } => "he-aac-v2",
            Self::AacOther(_) => "aac-other",
            Self::Alac => "alac",
            Self::Flac => "flac",
            Self::Pcm => "pcm",
            Self::PcmFloat => "pcm-float",
            Self::PcmOther => "pcm-other",
            Self::Vorbis => "vorbis",
            Self::Opus => "opus",
            Self::Unknown => "unknown",
        }
    }

    /// Lossy codecs (where a sample-exact comparison with the source is not
    /// meaningful).
    pub fn is_lossy(self) -> bool {
        matches!(
            self,
            Self::Mp1 | Self::Mp2 | Self::Mp3 | Self::AacLc | Self::HeAac { .. } | Self::AacOther(_) | Self::Vorbis | Self::Opus
        )
    }
}

/// Timing case (spec section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingCase {
    Mp3NoInfo,
    Mp3XingNoLame,
    Mp3Vbri,
    Mp3LavcTag,
    Mp3LameTagBadCrc,
    Mp3LameTag,
    Mp4Priming,
    Mp4NoPriming,
    Adts,
    Lossless,
    CafPakt,
    OggVorbis,
    OggOpus,
    Unknown,
}

impl TimingCase {
    /// The label used in reports and timing tables (spec section 2).
    pub fn label(self) -> &'static str {
        match self {
            Self::Mp3NoInfo | Self::Mp3XingNoLame | Self::Mp3Vbri => "mp3-A",
            Self::Mp3LavcTag => "mp3-B",
            Self::Mp3LameTagBadCrc => "mp3-C",
            Self::Mp3LameTag => "mp3-D",
            Self::Mp4Priming => "mp4-priming",
            Self::Mp4NoPriming => "mp4-none",
            Self::Adts => "adts",
            Self::Lossless => "lossless",
            Self::CafPakt => "caf-pakt",
            Self::OggVorbis => "ogg-vorbis",
            Self::OggOpus => "ogg-opus",
            Self::Unknown => "unknown",
        }
    }

    /// A more specific name than [`Self::label`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mp3NoInfo => "mp3-no-info",
            Self::Mp3XingNoLame => "mp3-xing-no-lame",
            Self::Mp3Vbri => "mp3-vbri",
            Self::Mp3LavcTag => "mp3-lavc-tag",
            Self::Mp3LameTagBadCrc => "mp3-lame-tag-bad-crc",
            Self::Mp3LameTag => "mp3-lame-tag",
            other => other.label(),
        }
    }
}

/// Where the priming value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimingSource {
    None,
    LameTag,
    EditList,
    ITunSmpb,
    CafPakt,
    OpusHead,
}

/// Result of [`probe_path`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeInfo {
    pub container: Container,
    pub codec: Codec,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    /// Bits per sample of the source, for PCM-like codecs.
    pub bits_per_sample: Option<u16>,
    /// Length in frames on the `decoder_output` basis, when known.
    pub frames_hint: Option<u64>,
    /// Whether `frames_hint` comes from an exact header field.
    pub duration_exact: bool,
    pub timing_case: TimingCase,
    /// Frames to drop from the decoder output to reach canonical frame 0.
    pub priming: u64,
    /// Trailing padding frames, when declared.
    pub padding: Option<u64>,
    /// Exact canonical length, when declared and consistent.
    pub valid_frames: Option<u64>,
    pub priming_source: PrimingSource,
    pub mp3: Option<Mp3Details>,
    pub mp4: Option<Mp4Details>,
    /// Ogg: the granule position of the first page that completes an audio
    /// packet (spec 7.1), reported only.
    pub ogg_first_granule: Option<u64>,
}

impl ProbeInfo {
    pub(crate) fn new(container: Container) -> Self {
        Self {
            container,
            codec: Codec::Unknown,
            sample_rate: None,
            channels: None,
            bits_per_sample: None,
            frames_hint: None,
            duration_exact: false,
            timing_case: TimingCase::Unknown,
            priming: 0,
            padding: None,
            valid_frames: None,
            priming_source: PrimingSource::None,
            mp3: None,
            mp4: None,
            ogg_first_granule: None,
        }
    }

    /// Length in frames on the given basis, when known.
    pub fn frames(&self, basis: crate::Basis) -> Option<u64> {
        match basis {
            crate::Basis::DecoderOutput => self.frames_hint,
            crate::Basis::Canonical => self
                .valid_frames
                .or_else(|| self.frames_hint.map(|f| f.saturating_sub(self.priming))),
        }
    }

    /// Duration in seconds on the given basis, when known.
    pub fn duration_seconds(&self, basis: crate::Basis) -> Option<f64> {
        let rate = self.sample_rate.filter(|&r| r > 0)?;
        Some(self.frames(basis)? as f64 / f64::from(rate))
    }

    /// Clears priming data that does not fit the declared length. Applied at
    /// the end of every container probe.
    fn sanitize(&mut self) {
        if let (Some(total), Some(valid)) = (self.frames_hint, self.valid_frames) {
            if self.duration_exact && self.priming.saturating_add(valid) > total {
                self.valid_frames = None;
            }
        }
    }
}

/// Probes a file. Only reads; never writes.
pub fn probe_path(path: &Path) -> Result<ProbeInfo> {
    let mut file = File::open(path)?;
    probe_reader(&mut file)
}

/// Probes any seekable byte source.
pub fn probe_reader<R: Read + Seek>(reader: &mut R) -> Result<ProbeInfo> {
    let mut src = io::Src::new(reader)?;
    let mut head = [0u8; 16];
    let n = src.read_at_most(0, &mut head)?;
    let head = &head[..n];

    let mut info = if head.starts_with(b"RIFF") || head.starts_with(b"RF64") {
        riff::probe_wav(&mut src)?
    } else if head.starts_with(b"FORM") {
        riff::probe_aiff(&mut src)?
    } else if head.starts_with(b"fLaC") {
        flac::probe(&mut src, 0)?
    } else if head.starts_with(b"OggS") {
        ogg::probe(&mut src)?
    } else if head.starts_with(b"caff") {
        caf::probe(&mut src)?
    } else if head.len() >= 8 && &head[4..8] == b"ftyp" {
        mp4::probe(&mut src)?
    } else {
        // ID3v2-prefixed FLAC, MPEG audio or ADTS.
        let start = mpeg::skip_id3v2(&mut src, 0)?;
        let mut magic = [0u8; 4];
        let m = src.read_at_most(start, &mut magic)?;
        if m == 4 && &magic == b"fLaC" {
            flac::probe(&mut src, start)?
        } else {
            mpeg::probe(&mut src, start)?
        }
    };
    info.sanitize();
    Ok(info)
}

#[cfg(test)]
mod tests;
