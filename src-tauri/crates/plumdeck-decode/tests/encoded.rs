//! Encoded fixtures produced at test time with ffmpeg (and lame/afconvert
//! when installed) into a temp dir; skipped with a note when ffmpeg is
//! missing. ffmpeg's own decode is used as a black-box reference.
//!
//! Run with `--nocapture` to see the per-fixture metrics.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use plumdeck_decode::{decode_to_pages, probe_path, Basis, Codec, Container, DecodeError, DecodeOptions, TimingCase, Unsupported};

/// Lags searched when aligning against the source.
const MAX_LAG: i64 = 2600;
const WINDOW: usize = 8192;

struct Fixtures {
    dir: tempfile::TempDir,
    ffmpeg: PathBuf,
    lame: Option<PathBuf>,
    afconvert: Option<PathBuf>,
    sox: Option<PathBuf>,
}

impl Fixtures {
    fn src(&self) -> PathBuf {
        self.dir.path().join("src.wav")
    }

    /// Runs `tool` with `{in}`/`{out}` placeholders; returns the output path.
    fn make(&self, tool: &Path, name: &str, args: &[&str]) -> Option<PathBuf> {
        let out = self.dir.path().join(name);
        if out.is_file() {
            return Some(out);
        }
        let src = self.src();
        let (src_s, out_s) = (src.to_str()?, out.to_str()?);
        let args: Vec<String> = args.iter().map(|a| a.replace("{in}", src_s).replace("{out}", out_s)).collect();
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run(tool, &refs).then_some(out)
    }

    fn ff(&self, name: &str, codec_args: &[&str]) -> Option<PathBuf> {
        let mut args = vec!["-v", "error", "-y", "-i", "{in}"];
        args.extend_from_slice(codec_args);
        args.push("{out}");
        let ffmpeg = self.ffmpeg.clone();
        self.make(&ffmpeg, name, &args)
    }

    fn lame(&self, name: &str, args: &[&str]) -> Option<PathBuf> {
        let lame = self.lame.clone()?;
        let mut a = vec!["--quiet"];
        a.extend_from_slice(args);
        a.extend_from_slice(&["{in}", "{out}"]);
        self.make(&lame, name, &a)
    }

    fn afconvert(&self, name: &str, args: &[&str]) -> Option<PathBuf> {
        let tool = self.afconvert.clone()?;
        let mut a = args.to_vec();
        a.extend_from_slice(&["{in}", "{out}"]);
        self.make(&tool, name, &a)
    }
}

/// Fresh fixtures per test (encoding is quick), removed when dropped.
fn fixtures() -> Option<Fixtures> {
    let Some(ffmpeg) = ffmpeg() else {
        eprintln!("ffmpeg not found: encoded fixture tests skipped");
        return None;
    };
    let dir = tempfile::tempdir().ok()?;
    let f = Fixtures { dir, ffmpeg, lame: find_tool("lame"), afconvert: find_tool("afconvert"), sox: find_tool("sox") };
    write_source_wav(&f.src());
    Some(f)
}

fn decode(path: &Path, basis: Basis) -> (Vec<f32>, u64) {
    let d = decode_to_pages(path, &DecodeOptions { basis, ..DecodeOptions::default() }).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(d.decode_errors, 0);
    let frames = d.frames();
    (d.pages.to_interleaved(), frames)
}

/// Alignment of decoded audio against the source: `out[n + lag] ~ src[n]`.
fn lag_vs_source(out_mono: &[f32]) -> i64 {
    let src = to_f32_mono(&source());
    best_lag(&src, out_mono, CLICK - 1000, WINDOW, MAX_LAG)
}

struct Report {
    lag: i64,
    frames: u64,
    ffmpeg_lag: Option<i64>,
    ffmpeg_err: Option<f32>,
    ffmpeg_corr: Option<f64>,
    ffmpeg_len: Option<usize>,
}

/// Decodes on the canonical basis and measures it against the source and
/// against ffmpeg.
fn measure(f: &Fixtures, path: &Path) -> Report {
    let (out, frames) = decode(path, Basis::Canonical);
    let mono = mono_of(&out, 2);
    let lag = lag_vs_source(&mono);
    let mut r = Report { lag, frames, ffmpeg_lag: None, ffmpeg_err: None, ffmpeg_corr: None, ffmpeg_len: None };
    if let Some(reference) = ffmpeg_decode(&f.ffmpeg, path, 2) {
        let ref_mono = mono_of(&reference, 2);
        r.ffmpeg_lag = Some(best_lag(&ref_mono, &mono, CLICK - 1000, WINDOW, MAX_LAG));
        let (err, corr) = compare(&out, &reference);
        r.ffmpeg_err = Some(err);
        r.ffmpeg_corr = Some(corr);
        r.ffmpeg_len = Some(reference.len() / 2);
    }
    eprintln!(
        "{:<22} lag_vs_src={:>5} frames={:>7} | ffmpeg: lag={:?} frames={:?} max_err={:?} corr={:?}",
        path.file_name().unwrap().to_string_lossy(),
        r.lag,
        r.frames,
        r.ffmpeg_lag,
        r.ffmpeg_len,
        r.ffmpeg_err,
        r.ffmpeg_corr
    );
    r
}

fn assert_matches_ffmpeg(r: &Report, max_err: f32) {
    assert_eq!(r.ffmpeg_lag, Some(0), "ffmpeg alignment");
    assert_eq!(r.ffmpeg_len, Some(r.frames as usize), "ffmpeg length");
    assert!(r.ffmpeg_err.unwrap() <= max_err, "ffmpeg max err {:?}", r.ffmpeg_err);
    assert!(r.ffmpeg_corr.unwrap() > 0.9999, "ffmpeg corr {:?}", r.ffmpeg_corr);
}

fn assert_lossless(path: &Path, container: Container, codec: Codec) {
    let info = probe_path(path).unwrap();
    assert_eq!((info.container, info.codec), (container, codec), "{}", path.display());
    assert_eq!(info.frames(Basis::Canonical), Some(LEN as u64), "{}: {info:?}", path.display());
    assert_eq!(info.sample_rate, Some(RATE));
    assert_eq!(info.channels, Some(2));
    let (out, frames) = decode(path, Basis::Canonical);
    assert_eq!(frames, LEN as u64);
    assert_eq!(out, to_f32_interleaved(&source()), "{}", path.display());
}

#[test]
fn lossless_formats_are_bit_exact() {
    let Some(f) = fixtures() else { return };
    assert_lossless(&f.ff("flac.flac", &["-c:a", "flac"]).unwrap(), Container::Flac, Codec::Flac);
    assert_lossless(&f.ff("aiff.aiff", &["-c:a", "pcm_s16be"]).unwrap(), Container::Aiff, Codec::Pcm);
    assert_lossless(&f.ff("alac.m4a", &["-c:a", "alac"]).unwrap(), Container::Mp4, Codec::Alac);
    assert_lossless(&f.ff("alac.caf", &["-c:a", "alac"]).unwrap(), Container::Caf, Codec::Alac);
    assert_lossless(&f.ff("pcm.caf", &["-c:a", "pcm_s16be"]).unwrap(), Container::Caf, Codec::Pcm);
    // FLAC with a leading ID3v2 tag.
    assert_lossless(&f.ff("id3.flac", &["-c:a", "flac", "-write_id3v2", "1", "-metadata", "title=x"]).unwrap(), Container::Flac, Codec::Flac);
}

#[test]
fn mp3_lame_tag_canonical_is_sample_aligned() {
    let Some(f) = fixtures() else { return };
    let mut fixtures = vec![("ffmpeg-cbr.mp3", f.ff("ffmpeg-cbr.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"]), TimingCase::Mp3LavcTag)];
    if f.lame.is_some() {
        fixtures.push(("lame-cbr.mp3", f.lame("lame-cbr.mp3", &["-b", "192"]), TimingCase::Mp3LameTag));
        fixtures.push(("lame-vbr.mp3", f.lame("lame-vbr.mp3", &["-V", "2"]), TimingCase::Mp3LameTag));
    } else {
        eprintln!("lame not found: LAME CLI fixtures skipped");
        fixtures.push(("ffmpeg-vbr.mp3", f.ff("ffmpeg-vbr.mp3", &["-c:a", "libmp3lame", "-q:a", "2"]), TimingCase::Mp3LavcTag));
    }
    for (name, path, case) in fixtures {
        let path = path.unwrap_or_else(|| panic!("{name} not produced"));
        let info = probe_path(&path).unwrap();
        assert_eq!((info.container, info.codec, info.timing_case), (Container::MpegAudio, Codec::Mp3, case), "{name}");
        let lame = info.mp3.as_ref().unwrap().lame.clone().unwrap();
        assert_eq!(info.priming, u64::from(lame.enc_delay) + 529);
        assert_eq!(info.valid_frames, Some(LEN as u64), "{name}: declared length");

        let r = measure(&f, &path);
        assert_eq!(r.lag, 0, "{name}: canonical start");
        assert_eq!(r.frames, LEN as u64, "{name}: canonical length");
        assert_matches_ffmpeg(&r, 1e-3);
        let (out, _) = decode(&path, Basis::Canonical);
        let click = peak_index(&mono_of(&out, 2), CLICK - 1000..CLICK + NOISE_GAP - 50);
        assert!(click.abs_diff(CLICK) <= 1, "{name}: click at {click}");

        // decoder_output = canonical + priming, and its length is frames * spf.
        let (raw, raw_frames) = decode(&path, Basis::DecoderOutput);
        assert_eq!(lag_vs_source(&mono_of(&raw, 2)), info.priming as i64, "{name}: decoder_output start");
        assert_eq!(Some(raw_frames), info.frames_hint, "{name}: decoder_output length");
    }
}

#[test]
fn mp3_without_xing_and_bad_crc_and_vbri() {
    let Some(f) = fixtures() else { return };
    let Some(cbr) = f.lame("lame-cbr.mp3", &["-b", "192"]).or_else(|| f.ff("ffmpeg-cbr.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"])) else {
        panic!("no mp3 encoder")
    };
    let tagged = probe_path(&cbr).unwrap();
    let delay = i64::from(tagged.mp3.as_ref().unwrap().lame.as_ref().unwrap().enc_delay);

    // No Xing/Info frame: canonical == decoder output, which starts with the
    // encoder delay plus the 529-sample decoder delay.
    let noxing = match &f.lame {
        Some(_) => f.lame("lame-noxing.mp3", &["-t", "-b", "192"]),
        None => f.ff("ffmpeg-noxing.mp3", &["-c:a", "libmp3lame", "-b:a", "192k", "-write_xing", "0"]),
    }
    .unwrap();
    let info = probe_path(&noxing).unwrap();
    assert_eq!(info.timing_case, TimingCase::Mp3NoInfo);
    assert_eq!(info.timing_case.label(), "mp3-A");
    assert_eq!(info.priming, 0);
    assert!(!info.duration_exact);
    let r = measure(&f, &noxing);
    assert_eq!(r.lag, delay + 529, "decoder output start without info frame");
    assert_matches_ffmpeg(&r, 1e-3);
    let estimate = info.frames_hint.unwrap();
    assert!(estimate.abs_diff(r.frames) <= 1152, "CBR estimate {estimate} vs {}", r.frames);

    // Corrupted LAME CRC: case C, same priming (spec 2).
    let mut bytes = std::fs::read(&cbr).unwrap();
    let off = tagged.mp3.as_ref().unwrap().audio_offset as usize + 36 + 120 + 34;
    bytes[off] ^= 0x5A;
    let bad = f.dir.path().join("badcrc.mp3");
    std::fs::write(&bad, &bytes).unwrap();
    let info = probe_path(&bad).unwrap();
    assert_eq!(info.timing_case, TimingCase::Mp3LameTagBadCrc);
    assert_eq!(info.priming, tagged.priming);
    let r = measure(&f, &bad);
    assert_eq!(r.lag, 0, "case C canonical start");
    assert_eq!(r.frames, LEN as u64);

    // VBRI: rewrite the first audio frame of the no-Xing file as a VBRI
    // frame. It is a header frame, so decoder output starts one frame later.
    let mut bytes = std::fs::read(&noxing).unwrap();
    let first = probe_path(&noxing).unwrap().mp3.unwrap().audio_offset as usize;
    let frames = (r.frames / 1152) as u32;
    bytes[first + 4..first + 36].fill(0);
    bytes[first + 36..first + 40].copy_from_slice(b"VBRI");
    bytes[first + 40..first + 42].copy_from_slice(&1u16.to_be_bytes());
    bytes[first + 42..first + 44].copy_from_slice(&1105u16.to_be_bytes());
    bytes[first + 46..first + 50].copy_from_slice(&0u32.to_be_bytes());
    bytes[first + 50..first + 54].copy_from_slice(&frames.to_be_bytes());
    let vbri = f.dir.path().join("vbri.mp3");
    std::fs::write(&vbri, &bytes).unwrap();
    let info = probe_path(&vbri).unwrap();
    assert_eq!(info.timing_case, TimingCase::Mp3Vbri);
    assert_eq!(info.mp3.as_ref().unwrap().vbri_delay, Some(1105));
    let (out, _) = decode(&vbri, Basis::Canonical);
    assert_eq!(lag_vs_source(&mono_of(&out, 2)), delay + 529 - 1152, "VBRI frame is not audio");
}

#[test]
fn mp3_mpeg2_matches_ffmpeg() {
    let Some(f) = fixtures() else { return };
    let path = match &f.lame {
        Some(_) => f.lame("lame-22k.mp3", &["-b", "64", "--resample", "22.05"]),
        None => f.ff("ffmpeg-22k.mp3", &["-c:a", "libmp3lame", "-b:a", "64k", "-ar", "22050"]),
    }
    .unwrap();
    let info = probe_path(&path).unwrap();
    assert_eq!(info.sample_rate, Some(22_050));
    assert_eq!(info.mp3.as_ref().unwrap().samples_per_frame, 576);
    assert!(info.mp3.as_ref().unwrap().lame.is_some());
    let r = measure(&f, &path);
    assert_matches_ffmpeg(&r, 1e-3);
    assert_eq!(r.frames, (LEN as u64).div_ceil(2));
}

#[test]
fn aac_lc_m4a_canonical_is_sample_aligned() {
    let Some(f) = fixtures() else { return };
    let mut cases = vec![f.ff("ffmpeg-aac.m4a", &["-c:a", "aac", "-b:a", "192k"]).unwrap()];
    if let Some(p) = f.afconvert("afconvert-aac.m4a", &["-f", "m4af", "-d", "aac", "-b", "192000"]) {
        cases.push(p);
    }
    for path in cases {
        let info = probe_path(&path).unwrap();
        eprintln!("{}: {:?} priming={} via {:?} valid={:?} mp4={:?}", path.display(), info.timing_case, info.priming, info.priming_source, info.valid_frames, info.mp4);
        assert_eq!((info.container, info.codec, info.timing_case), (Container::Mp4, Codec::AacLc, TimingCase::Mp4Priming));
        let r = measure(&f, &path);
        assert_eq!(r.lag, 0, "{}: canonical start", path.display());
        assert_eq!(r.frames, LEN as u64, "{}: canonical length", path.display());
        let (out, _) = decode(&path, Basis::Canonical);
        let click = peak_index(&mono_of(&out, 2), CLICK - 1000..CLICK + NOISE_GAP - 50);
        assert!(click.abs_diff(CLICK) <= 2, "click at {click}");
        // ffmpeg applies the edit list; with iTunSMPB-only files it may not.
        if info.priming_source == plumdeck_decode::PrimingSource::EditList {
            assert_matches_ffmpeg(&r, 5e-3);
        }
        let (raw, _) = decode(&path, Basis::DecoderOutput);
        assert_eq!(lag_vs_source(&mono_of(&raw, 2)), info.priming as i64);
    }
}

#[test]
fn ogg_vorbis_and_adts_decode() {
    let Some(f) = fixtures() else { return };
    // Prefer libvorbis (through sox). ffmpeg's native encoder is
    // experimental: on stereo input its second channel decodes differently
    // in ffmpeg and in symphonia (symphonia's is closer to the source), so
    // only alignment and length are checked for it.
    let libvorbis = f.sox.clone().and_then(|sox| f.make(&sox, "libvorbis.ogg", &["{in}", "-C", "6", "{out}"]));
    let (ogg, exact) = match libvorbis {
        Some(p) => (p, true),
        None => (f.ff("vorbis.ogg", &["-c:a", "vorbis", "-strict", "-2", "-q:a", "6"]).unwrap(), false),
    };
    let info = probe_path(&ogg).unwrap();
    assert_eq!((info.container, info.codec, info.timing_case), (Container::Ogg, Codec::Vorbis, TimingCase::OggVorbis));
    // The declared length (last granule) may exceed the source by less than
    // a block when the encoder does not trim the end.
    let declared = info.valid_frames.unwrap();
    assert!(declared >= LEN as u64 && declared - (LEN as u64) < 2048, "declared {declared}");
    let r = measure(&f, &ogg);
    assert_eq!(r.lag, 0);
    assert_eq!(r.frames, declared);
    if exact {
        assert_matches_ffmpeg(&r, 5e-3);
    } else {
        assert_eq!((r.ffmpeg_lag, r.ffmpeg_len), (Some(0), Some(r.frames as usize)));
    }

    // ADTS has no priming field: canonical == decoder output, which carries
    // the encoder's 1024-frame priming.
    let adts = f.ff("adts.aac", &["-c:a", "aac", "-b:a", "192k"]).unwrap();
    let info = probe_path(&adts).unwrap();
    assert_eq!((info.container, info.codec, info.timing_case), (Container::Adts, Codec::AacLc, TimingCase::Adts));
    let r = measure(&f, &adts);
    assert_eq!(r.lag, 1024);
    assert_eq!(Some(r.frames), info.frames_hint);
}

#[test]
fn unsupported_codecs_are_typed() {
    let Some(f) = fixtures() else { return };
    let opus = f.ff("opus.opus", &["-c:a", "libopus", "-b:a", "96k"]).unwrap();
    let info = probe_path(&opus).unwrap();
    assert_eq!((info.codec, info.timing_case), (Codec::Opus, TimingCase::OggOpus));
    assert_eq!(info.priming, 312);
    assert!(matches!(decode_to_pages(&opus, &DecodeOptions::default()), Err(DecodeError::Unsupported(Unsupported::Opus))));

    let mut he = Vec::new();
    if let Some(p) = f.afconvert("afconvert-he.m4a", &["-f", "m4af", "-d", "aach", "-b", "64000"]) {
        he.push(p);
    }
    // aac_at profiles 4 = HE-AAC, 28 = HE-AAC v2 (macOS only).
    for (name, profile) in [("aac-at-he.m4a", "4"), ("aac-at-hev2.m4a", "28")] {
        if let Some(p) = f.ff(name, &["-c:a", "aac_at", "-profile:a", profile, "-b:a", "48k"]) {
            he.push(p);
        }
    }
    if he.is_empty() {
        eprintln!("no HE-AAC encoder: HE-AAC fixtures skipped");
    }
    for path in he {
        let info = probe_path(&path).unwrap();
        eprintln!("{}: {:?} {:?} rate={:?}", path.display(), info.codec, info.mp4.as_ref().map(|m| (m.audio_object_type, m.asc_sample_rate, m.entry_sample_rate)), info.sample_rate);
        assert!(matches!(info.codec, Codec::HeAac { .. }), "{}", path.display());
        assert!(matches!(decode_to_pages(&path, &DecodeOptions::default()), Err(DecodeError::Unsupported(Unsupported::HeAac { .. }))));
    }
}
