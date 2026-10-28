//! Decoding (W1-05): symphonia with its own gapless trimming disabled, then
//! basis alignment from the probe (spec section 1).

use std::fs::File;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::error::{DecodeError, Result, Unsupported};
use crate::pages::{PcmPages, DEFAULT_PAGE_FRAMES};
use crate::probe::{probe_path, Codec, Container, ProbeInfo};
use crate::resample::{default_resampler, ResamplerFactory};

/// Time basis of decoded output (spec section 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Basis {
    /// Frame 0 is the first frame the codec produces; nothing is trimmed.
    DecoderOutput,
    /// Encoder delay/priming removed and the declared length applied.
    Canonical,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DecoderOutput => "decoder_output",
            Self::Canonical => "canonical",
        }
    }
}

impl FromStr for Basis {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "decoder_output" => Ok(Self::DecoderOutput),
            "canonical" => Ok(Self::Canonical),
            other => Err(format!("unknown basis {other:?}")),
        }
    }
}

/// Consecutive undecodable packets tolerated before giving up.
const MAX_CONSECUTIVE_ERRORS: u32 = 64;
/// Largest silent gap inserted for one undecodable packet.
const MAX_GAP_FRAMES: u64 = 1 << 16;

fn map_sym(err: SymError) -> DecodeError {
    match err {
        SymError::IoError(e) => DecodeError::Io(e),
        SymError::Unsupported(msg) => DecodeError::Unsupported(Unsupported::Other(msg.to_owned())),
        SymError::DecodeError(msg) | SymError::LimitError(msg) => DecodeError::Malformed(msg.to_owned()),
        SymError::SeekError(_) => DecodeError::malformed("seek error"),
        SymError::ResetRequired => DecodeError::malformed("stream reset required"),
        other => DecodeError::Malformed(other.to_string()),
    }
}

/// Rejects codecs this crate knows symphonia cannot decode correctly.
fn check_supported(info: &ProbeInfo) -> Result<()> {
    match info.codec {
        Codec::HeAac { ps, implicit } => Err(DecodeError::Unsupported(Unsupported::HeAac { ps, implicit })),
        Codec::Opus => Err(DecodeError::Unsupported(Unsupported::Opus)),
        Codec::AacOther(aot) => Err(DecodeError::Unsupported(Unsupported::Codec(format!("aac object type {aot}")))),
        _ => Ok(()),
    }
}

/// A streaming decoder aligned to a [`Basis`].
pub struct Decoder {
    info: ProbeInfo,
    basis: Basis,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    /// Frames still to drop from the start.
    skip: u64,
    /// Frames still to emit, when the length is declared.
    remaining: Option<u64>,
    raw_frames: u64,
    emitted: u64,
    /// Vorbis: symphonia emits the first packet's lapped half, which a
    /// conforming decoder discards (the packet's `trim_start`); drop it on
    /// every basis.
    drop_lapped: bool,
    eof: bool,
    consecutive_errors: u32,
    decode_errors: u64,
    scratch: Vec<f32>,
    planes: Vec<Vec<f32>>,
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoder")
            .field("basis", &self.basis)
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("emitted", &self.emitted)
            .finish_non_exhaustive()
    }
}

/// Opens a file for decoding on the given basis. Only reads the file.
pub fn open(path: &Path, basis: Basis) -> Result<Decoder> {
    let info = match probe_path(path) {
        Ok(info) => info,
        // Let symphonia try what the probe could not classify; such streams
        // have no timing information (canonical == decoder output).
        Err(DecodeError::Malformed(_) | DecodeError::Unsupported(Unsupported::Container)) => ProbeInfo::new(Container::Unknown),
        Err(e) => return Err(e),
    };
    check_supported(&info)?;

    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format = symphonia::default::get_probe()
        .probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| match e {
            SymError::Unsupported(_) => DecodeError::Unsupported(Unsupported::Container),
            other => map_sym(other),
        })?;
    Decoder::new(info, format, basis)
}

impl Decoder {
    fn new(mut info: ProbeInfo, format: Box<dyn FormatReader>, basis: Basis) -> Result<Self> {
        let track = format.default_track(TrackType::Audio).ok_or(DecodeError::NoAudioTrack)?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(DecodeError::NoAudioTrack)?
            .clone();
        let opts = AudioDecoderOptions::default().gapless(false);
        let decoder = symphonia::default::get_codecs().make_audio_decoder(&params, &opts).map_err(|e| match e {
            SymError::Unsupported(msg) => DecodeError::Unsupported(Unsupported::Codec(msg.to_owned())),
            other => map_sym(other),
        })?;
        let sample_rate = params.sample_rate.or(info.sample_rate).filter(|&r| r > 0).ok_or_else(|| DecodeError::malformed("unknown sample rate"))?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .filter(|&c| c > 0)
            .or(info.channels.map(usize::from))
            .filter(|&c| c > 0)
            .ok_or_else(|| DecodeError::malformed("unknown channel count"))?;
        if info.container == Container::Unknown {
            info.sample_rate = Some(sample_rate);
            info.channels = u16::try_from(channels).ok();
        }
        let (skip, remaining) = match basis {
            Basis::DecoderOutput => (0, None),
            Basis::Canonical => (info.priming, info.valid_frames),
        };
        Ok(Self {
            info,
            basis,
            format,
            decoder,
            track_id,
            sample_rate,
            channels,
            skip,
            remaining,
            raw_frames: 0,
            emitted: 0,
            drop_lapped: params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_VORBIS,
            eof: false,
            consecutive_errors: 0,
            decode_errors: 0,
            scratch: Vec::new(),
            planes: Vec::new(),
        })
    }

    pub fn info(&self) -> &ProbeInfo {
        &self.info
    }

    pub fn basis(&self) -> Basis {
        self.basis
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames emitted so far on this decoder's basis.
    pub fn position(&self) -> u64 {
        self.emitted
    }

    /// Frames the codec produced so far (`decoder_output` basis).
    pub fn raw_frames(&self) -> u64 {
        self.raw_frames
    }

    /// Packets that failed to decode and were replaced by silence.
    pub fn decode_errors(&self) -> u64 {
        self.decode_errors
    }

    /// Decodes the next block into `out` (interleaved, cleared first).
    /// Returns the number of frames; 0 means end of stream.
    pub fn read_block(&mut self, out: &mut Vec<f32>) -> Result<usize> {
        out.clear();
        let ch = self.channels;
        loop {
            if self.eof || self.remaining == Some(0) {
                return Ok(0);
            }
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) | Err(SymError::ResetRequired) => {
                    self.eof = true;
                    return Ok(0);
                }
                Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    self.eof = true;
                    return Ok(0);
                }
                Err(SymError::DecodeError(msg)) => {
                    self.note_error(msg)?;
                    continue;
                }
                Err(e) => return Err(map_sym(e)),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let frames = match self.decoder.decode(&packet) {
                Ok(buf) => {
                    self.consecutive_errors = 0;
                    let n = buf.frames();
                    if buf.spec().channels().count() == ch {
                        buf.copy_to_vec_interleaved(&mut self.scratch);
                    } else {
                        // Channel layout changed mid-stream: map onto the
                        // track's channel count (repeat the last plane).
                        buf.copy_to_vecs_planar(&mut self.planes);
                        self.scratch.clear();
                        self.scratch.reserve(n * ch);
                        for i in 0..n {
                            for c in 0..ch {
                                let plane = self.planes.get(c).or(self.planes.last());
                                self.scratch.push(plane.and_then(|p| p.get(i)).copied().unwrap_or(0.0));
                            }
                        }
                    }
                    n
                }
                Err(SymError::DecodeError(msg)) => {
                    self.note_error(msg)?;
                    // Keep the timeline: replace the packet by silence.
                    let n = packet.block_dur().get().min(MAX_GAP_FRAMES) as usize;
                    self.scratch.clear();
                    self.scratch.resize(n * ch, 0.0);
                    n
                }
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(map_sym(e)),
            };
            let mut start = 0usize;
            if self.drop_lapped {
                start = packet.trim_start.get().min(frames as u64) as usize;
            }
            self.raw_frames += (frames - start) as u64;
            if self.skip > 0 {
                let drop = self.skip.min((frames - start) as u64) as usize;
                self.skip -= drop as u64;
                start += drop;
            }
            let mut end = frames;
            if let Some(rem) = self.remaining {
                end = end.min(start + rem.min((frames - start) as u64) as usize);
            }
            if end > start {
                out.extend_from_slice(&self.scratch[start * ch..end * ch]);
                let n = end - start;
                self.emitted += n as u64;
                if let Some(rem) = self.remaining.as_mut() {
                    *rem -= n as u64;
                }
                return Ok(n);
            }
        }
    }

    fn note_error(&mut self, msg: &str) -> Result<()> {
        self.decode_errors += 1;
        self.consecutive_errors += 1;
        if self.consecutive_errors > MAX_CONSECUTIVE_ERRORS {
            return Err(DecodeError::Malformed(format!("too many consecutive decode errors: {msg}")));
        }
        Ok(())
    }
}

/// Options for [`decode_to_pages`].
#[derive(Debug, Clone)]
pub struct DecodeOptions {
    pub basis: Basis,
    /// Resample to this rate (`None` keeps the source rate).
    pub target_rate: Option<u32>,
    /// Average all channels into one.
    pub mono: bool,
    /// Stop after this many seconds of output (the result is marked
    /// truncated).
    pub max_seconds: f64,
    pub page_frames: usize,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self { basis: Basis::Canonical, target_rate: None, mono: false, max_seconds: 1800.0, page_frames: DEFAULT_PAGE_FRAMES }
    }
}

/// Decoded audio in pages, with the probe result.
#[derive(Debug)]
pub struct Decoded {
    pub info: ProbeInfo,
    pub basis: Basis,
    pub pages: Arc<PcmPages>,
    /// Packets replaced by silence.
    pub decode_errors: u64,
    /// Frames the codec produced at the source rate (`decoder_output`).
    pub raw_frames: u64,
}

impl Decoded {
    pub fn sample_rate(&self) -> u32 {
        self.pages.sample_rate()
    }

    pub fn channels(&self) -> usize {
        self.pages.channels()
    }

    pub fn frames(&self) -> u64 {
        self.pages.frames_available()
    }

    pub fn truncated(&self) -> bool {
        self.pages.is_truncated()
    }
}

/// Decodes a whole file into pages with the built-in resampler.
pub fn decode_to_pages(path: &Path, opts: &DecodeOptions) -> Result<Decoded> {
    decode_to_pages_with(path, opts, &default_resampler)
}

/// Decodes a whole file into pages using `make_resampler` when resampling.
pub fn decode_to_pages_with(path: &Path, opts: &DecodeOptions, make_resampler: &ResamplerFactory) -> Result<Decoded> {
    let mut dec = open(path, opts.basis)?;
    let src_rate = dec.sample_rate();
    let out_ch = if opts.mono { 1 } else { dec.channels() };
    let out_rate = opts.target_rate.filter(|&r| r > 0).unwrap_or(src_rate);
    let max_seconds = if opts.max_seconds.is_finite() && opts.max_seconds > 0.0 { opts.max_seconds } else { 1800.0 };
    let max_frames = (max_seconds * f64::from(out_rate)).ceil() as u64;
    let (_, mut writer) = PcmPages::new(out_ch, out_rate, opts.page_frames, max_frames);
    let mut resampler = (out_rate != src_rate).then(|| make_resampler(out_ch, src_rate, out_rate));
    if let Some(r) = &resampler {
        if r.channels() != out_ch || r.output_rate() != out_rate || r.input_rate() != src_rate {
            return Err(DecodeError::malformed("resampler does not match the requested format"));
        }
    }

    let mut block = Vec::new();
    let mut mono = Vec::new();
    let mut resampled = Vec::new();
    let mut full = false;
    while !full {
        let n = dec.read_block(&mut block)?;
        if n == 0 {
            break;
        }
        let data: &[f32] = if opts.mono && dec.channels() > 1 {
            downmix(&block, dec.channels(), &mut mono);
            &mono
        } else {
            &block
        };
        if let Some(r) = resampler.as_mut() {
            resampled.clear();
            r.process(data, &mut resampled);
            full = !writer.push_interleaved(&resampled);
        } else {
            full = !writer.push_interleaved(data);
        }
    }
    if let (Some(r), false) = (resampler.as_mut(), full) {
        resampled.clear();
        r.finish(&mut resampled);
        writer.push_interleaved(&resampled);
    }
    let pages = writer.finish();
    Ok(Decoded { info: dec.info.clone(), basis: opts.basis, pages, decode_errors: dec.decode_errors, raw_frames: dec.raw_frames })
}

fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    let scale = 1.0 / channels as f32;
    out.extend(interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() * scale));
}

/// Mono PCM for analysis.
#[derive(Debug)]
pub struct MonoPcm {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub info: ProbeInfo,
    pub truncated: bool,
}

/// Decodes to mono at `target_rate` (or the source rate), at most
/// `max_seconds` long.
pub fn decode_mono(path: &Path, basis: Basis, target_rate: Option<u32>, max_seconds: f64) -> Result<MonoPcm> {
    let opts = DecodeOptions { basis, target_rate, mono: true, max_seconds, ..DecodeOptions::default() };
    let d = decode_to_pages(path, &opts)?;
    let truncated = d.truncated();
    Ok(MonoPcm { samples: d.pages.to_interleaved(), sample_rate: d.sample_rate(), info: d.info, truncated })
}
