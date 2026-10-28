//! # Decoder timing and header probing: clean-room specification
//!
//! This module holds no code. It is the specification that [`crate::probe`]
//! and [`crate::decode`] implement, written only from public format
//! documents (cited per section) and from black-box measurement. It does not
//! derive from `backend/infra/rekordbox_timing.py`, Mixxx or mp3guessenc, and
//! the implementation must not be changed by looking at them; those may only
//! be run as black boxes to compare results.
//!
//! ## 1. Time bases
//!
//! Every position this crate reports is a frame index (one frame = one
//! sample per channel) at the stream's own sample rate, on one of two bases:
//!
//! | basis | frame 0 is |
//! |---|---|
//! | `decoder_output` | the first PCM frame the codec produces for the first *audio* access unit of the stream, with no trimming at all. Header-only access units (the MP3 Xing/Info/VBRI frame, see 3.3) are not audio and produce nothing. |
//! | `canonical` | the first frame of the original signal that was fed to the encoder: `decoder_output` minus the stream's *priming* (encoder delay plus any decoder delay that the format's gapless convention counts in), and cut after `valid_frames` when the stream declares its exact length. When the file carries no priming information, `canonical == decoder_output` (case A). |
//!
//! So `canonical(t) = decoder_output(t) - priming`, with `priming = 0` when
//! unknown. A legacy basis (another decoder's frame 0) is a per-case constant
//! offset from these two; that table is measured elsewhere (W1-03, RB-12) and
//! is not part of this crate.
//!
//! ## 2. Timing cases
//!
//! The probe assigns exactly one case. Cases are defined by which header
//! elements are present, never by file name or encoder guesses beyond the
//! fields listed.
//!
//! | case | label | condition | priming | valid_frames |
//! |---|---|---|---|---|
//! | `Mp3NoInfo` | mp3-A | MPEG audio, first audio frame carries no Xing/Info/VBRI tag | 0 (unknown) | unknown |
//! | `Mp3XingNoLame` | mp3-A | Xing/Info tag without a LAME-format extension (3.4) | 0 (unknown) | unknown |
//! | `Mp3Vbri` | mp3-A | Fraunhofer VBRI tag (3.5) | 0 (unknown; the VBRI delay field is reported, not applied) | unknown |
//! | `Mp3LavcTag` | mp3-B | LAME-format extension whose encoder string starts with `Lavc` or `Lavf`, CRC valid | `enc_delay + 529` | `frames * spf - enc_delay - enc_padding` |
//! | `Mp3LameTagBadCrc` | mp3-C | LAME-format extension present but the tag CRC does not match | as B/D | as B/D |
//! | `Mp3LameTag` | mp3-D | LAME-format extension with any other encoder string (`LAME`, `GOGO`, ...), CRC valid | `enc_delay + 529` | as B |
//! | `Mp4Priming` | mp4-priming | MP4 track with a priming value from an edit list or iTunSMPB (4.3) | that value | from the same source |
//! | `Mp4NoPriming` | mp4-none | MP4 track with neither | 0 | unknown |
//! | `Adts` | adts | raw ADTS AAC (no priming field exists) | 0 | unknown |
//! | `Lossless` | lossless | WAV, AIFF/AIFC, FLAC, CAF without `pakt` priming | 0 | header length |
//! | `CafPakt` | caf-pakt | CAF with a `pakt` chunk (6) | `mPrimingFrames` | `mNumberValidFrames` |
//! | `OggVorbis` | ogg-vorbis | Ogg Vorbis (7.1) | 0 | last granule position |
//! | `OggOpus` | ogg-opus | Ogg Opus (7.2) | `pre_skip` | last granule - pre_skip |
//!
//! The labels mp3-A..D are this specification's own names. Matching them
//! against any other implementation's letters is a cross-check (W1-02), not
//! an assumption made here.
//!
//! **Case C decision.** A CRC mismatch means the 190 bytes before the CRC
//! field are not what the writer checksummed; it does not by itself make the
//! delay field wrong (encoders have shipped with CRC bugs, and the field sits
//! well inside the protected range). The extension is only accepted when its
//! encoder string is printable ASCII (3.4), which rules out reading random
//! frame bytes as a tag. With that guard, C applies the same priming as D,
//! and the case label keeps C distinguishable for measured legacy offsets.
//!
//! ## 3. MPEG audio (MP1/MP2/MP3)
//!
//! Sources: ISO/IEC 11172-3 and 13818-3 frame header layout (as summarised in
//! public references such as mpgedit's "MPEG Audio Frame Header"), the ID3v2.3
//! and ID3v2.4 structure documents (id3.org), the LAME "Mp3 info tag rev 1
//! specifications" (gabriel.mp3-tech.org/mp3infotag.html), the LAME
//! technical FAQ on encoder/decoder delay, and the public Xing VBR header and
//! Fraunhofer VBRI descriptions.
//!
//! ### 3.1 Leading tags
//!
//! An ID3v2 tag is `"ID3"`, major version (2..=4), revision, flags, then a
//! 4-byte *synchsafe* size (7 bits per byte, top bit must be 0). Its total
//! length is `10 + size`, plus 10 when flag bit 4 (footer present, v2.4) is
//! set. Several tags may follow one another; all are skipped. A synchsafe
//! byte with the top bit set makes the tag invalid and stops skipping.
//!
//! Trailing tags are excluded from the audio byte range when estimating a
//! CBR duration: ID3v1 (`"TAG"` in the last 128 bytes) and an APEv2 tag
//! ending in a 32-byte footer `"APETAGEX"` (its size field counts the items
//! plus footer; a header adds 32 more when flag bit 31 is set).
//!
//! ### 3.2 Frame sync
//!
//! A frame header is 32 bits: 11 sync bits set, version (`00` MPEG-2.5,
//! `10` MPEG-2, `11` MPEG-1, `01` reserved), layer (`01` III, `10` II,
//! `11` I, `00` reserved), protection bit (0 = 16-bit CRC follows), bitrate
//! index (0 free-format and 15 invalid are rejected), sample-rate index (3
//! reserved), padding bit, private bit, channel mode (`11` = mono).
//!
//! Frame length in bytes: layer I `(12 * bitrate / rate + pad) * 4`;
//! layer II and MPEG-1 layer III `144 * bitrate / rate + pad`;
//! MPEG-2/2.5 layer III `72 * bitrate / rate + pad`.
//! Samples per frame (`spf`): layer I 384, layer II 1152, layer III 1152 for
//! MPEG-1 and 576 for MPEG-2/2.5.
//!
//! The first frame is searched from the end of the ID3v2 tags, up to 1 MiB
//! further. A candidate is accepted when the next three frame headers follow
//! back to back with the same version, layer and sample rate (fewer only if
//! the file ends first). The byte where the frame was found is
//! `audio_offset`; bytes skipped before it are reported as `junk_bytes`.
//!
//! ### 3.3 Header-only first frame
//!
//! The Xing/Info and VBRI tags live inside the first frame, which then
//! carries no audio. That frame is not part of either time base: the first
//! audio frame is the one after it.
//!
//! ### 3.4 Xing/Info and the LAME-format extension
//!
//! The Xing tag (`"Xing"` for VBR, `"Info"` for CBR) starts right after the
//! side information: header (4) + CRC (2 if protected) + side info
//! (MPEG-1: 17 mono / 32 otherwise; MPEG-2/2.5: 9 mono / 17 otherwise).
//! After the 4-byte id: a 32-bit big-endian flags word, then, in this order,
//! only the fields whose flag is set: frames (bit 0, u32), bytes (bit 1,
//! u32), TOC (bit 2, 100 bytes), quality (bit 3, u32).
//!
//! The LAME-format extension follows directly (36 bytes):
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 9 | encoder string (e.g. `LAME3.100`, `Lavc61.19`) |
//! | 9 | 1 | tag revision (high nibble), VBR method (low) |
//! | 10 | 1 | lowpass / 100 Hz |
//! | 11 | 4 | peak signal (float) |
//! | 15 | 2 | radio replay gain |
//! | 17 | 2 | audiophile replay gain |
//! | 19 | 1 | encoding flags, ATH type |
//! | 20 | 1 | bitrate (ABR target or minimal) |
//! | 21 | 3 | encoder delay (high 12 bits), padding (low 12 bits) |
//! | 24 | 1 | misc |
//! | 25 | 1 | MP3 gain |
//! | 26 | 2 | preset and surround |
//! | 28 | 4 | music length (bytes) |
//! | 32 | 2 | music CRC |
//! | 34 | 2 | tag CRC |
//!
//! The extension is *present* when the frame is long enough to hold it and
//! the first 4 encoder-string bytes are printable ASCII (0x20..=0x7E) and not
//! all spaces. The tag CRC is CRC-16 (polynomial 0x8005 reflected, i.e.
//! 0xA001 LSB-first, initial value 0) over every frame byte from the frame
//! header up to, not including, the tag CRC field (190 bytes in the common
//! MPEG-1 stereo layout).
//!
//! **Priming.** The delay field counts the encoder's added samples only.
//! A layer III decoder adds a further fixed 528-sample filterbank delay plus
//! one sample of alignment, 529 in total (LAME technical FAQ; this crate
//! verifies the value by measurement in its tests). So:
//!
//! - `priming = enc_delay + 529`
//! - when the Xing frame count is present:
//!   `valid_frames = frames * spf - enc_delay - enc_padding`
//!   (the decoder-output length is `frames * spf`; the trailing
//!   `enc_padding - 529` frames, if positive, are padding)
//! - if `priming + valid_frames > frames * spf` the header is inconsistent:
//!   the values are kept in the report but `valid_frames` becomes unknown.
//!
//! The extension is only recognised in a Xing/Info frame, as defined by the
//! LAME tag specification.
//!
//! ### 3.5 VBRI
//!
//! `"VBRI"` at byte 36 of the first frame (32 after the header regardless of
//! version and channel mode): version u16, delay u16, quality u16, bytes u32,
//! frames u32, TOC entry count u16, scale u16, entry size u16, frames per
//! entry u16, TOC. The meaning of the delay field is not publicly specified
//! precisely enough to apply; it is reported as `vbri_delay` only.
//!
//! ### 3.6 Length
//!
//! - Xing frame count present: `decoder_output` length = `frames * spf`
//!   (exact).
//! - VBRI: `frames * spf` (exact as declared).
//! - Otherwise an estimate from the first frame's bitrate and the audio byte
//!   range (3.1); reported with `duration_exact = false`.
//!
//! ## 4. MP4 / M4A (ISO base media file format)
//!
//! Sources: ISO/IEC 14496-12 (box structure, `mvhd`, `tkhd`, `mdhd`,
//! `hdlr`, `stsd`, `edts`/`elst`), ISO/IEC 14496-14 (`esds`), ISO/IEC 14496-1
//! (descriptor tags and size encoding), ISO/IEC 14496-3 (AudioSpecificConfig),
//! Apple's QuickTime File Format reference (sound sample description v1/v2,
//! metadata `ilst`, freeform `----`/`mean`/`name`/`data`), Apple's ALAC
//! magic cookie description, and Apple's published gapless note on the
//! `iTunSMPB` tag.
//!
//! ### 4.1 Box walk
//!
//! A box is `size: u32`, `type: [u8; 4]`; size 1 means a u64 `largesize`
//! follows, size 0 means "to the end of the enclosing range". Containers
//! walked: `moov`, `trak`, `mdia`, `minf`, `stbl`, `edts`, `udta`, `meta`
//! (a full box in ISO files: 4 version/flags bytes come first; QuickTime
//! writes it without them, detected by a valid child box header at offset
//! 0), `ilst`. Everything else, including `mdat`, is skipped by seeking.
//! The first track whose `hdlr` handler type is `soun` is used.
//!
//! ### 4.2 Codec
//!
//! `stsd` holds the sample entry. `mp4a`: after the 8-byte SampleEntry
//! header and the 20-byte sound description (version u16 first; QuickTime
//! version 1 adds 16 bytes, version 2 adds 36), child boxes follow; `esds`
//! (directly or inside `wave`) holds ES_Descriptor (tag 3) ->
//! DecoderConfigDescriptor (tag 4, objectTypeIndication 0x40 = MPEG-4 audio,
//! 0x66..=0x68 = MPEG-2 AAC, 0x69/0x6B = MPEG-1/2 audio layer III) ->
//! DecoderSpecificInfo (tag 5) = AudioSpecificConfig.
//!
//! AudioSpecificConfig: object type (5 bits; 31 escapes to `32 + 6 bits`),
//! sampling-frequency index (4 bits; 15 escapes to a 24-bit rate), channel
//! configuration (4 bits). Object type 5 (SBR) or 29 (PS) is *explicit*
//! HE-AAC (v1/v2): an extension sampling-frequency index follows and then
//! the core object type. For object type 2 with a non-zero channel
//! configuration the GASpecificConfig (frame-length flag, depends-on-core
//! flag [+14 bits], extension flag) is skipped and, if at least 16 bits
//! remain, a sync extension `0x2b7` + object type 5 + sbrPresent flag is
//! *backward-compatible explicit* HE-AAC (and `0x548` + psPresent flag
//! marks PS).
//!
//! *Implicit* HE-AAC carries no ASC signal. It is suspected when the core
//! is AAC-LC and the sample entry rate is exactly twice the ASC rate.
//!
//! All three HE-AAC forms are reported as `Codec::HeAac` and decoding
//! returns `Unsupported::HeAac` (symphonia has no SBR; decoding the core only
//! would silently produce a half-band, half-rate signal).
//!
//! `alac`: the inner `alac` full box holds the 24-byte ALAC specific config
//! (frame length u32, compatible version, bit depth, pb, mb, kb, channels,
//! max run u16, max frame bytes u32, average bit rate u32, sample rate u32).
//!
//! ### 4.3 Priming
//!
//! - `elst` (in the audio `trak`): version 0 entries are
//!   `segment_duration u32, media_time i32, rate i16.i16`; version 1 uses
//!   `u64, i64`. `media_time = -1` is an empty edit (a presentation gap,
//!   recorded but not applied). The first non-empty entry gives
//!   `elst_priming = media_time` in `mdhd` timescale units and
//!   `elst_valid = segment_duration` in `mvhd` timescale units.
//! - `iTunSMPB` (freeform `----` item, mean `com.apple.iTunes`, name
//!   `iTunSMPB`): a text of space-separated hex words; word 2 is the
//!   priming, word 3 the padding and word 4 the original sample count.
//! - Values in a timescale other than the sample rate are converted with
//!   `value * sample_rate / timescale`, rounded to nearest.
//!
//! Precedence: an edit list with `elst_priming > 0` is structural (written by
//! the muxer that also wrote the samples), so it wins; otherwise iTunSMPB is
//! used when present; otherwise priming is 0. Both raw values are reported
//! so disagreements can be audited. The length of the `decoder_output` basis
//! is the sum of `stts` durations, approximated by `mdhd.duration`.
//!
//! ## 5. FLAC, WAV, AIFF
//!
//! Sources: the FLAC format specification (RFC 9639), Microsoft's RIFF/WAVE
//! and WAVE_FORMAT_EXTENSIBLE documentation, Apple's AIFF 1.3 and AIFF-C
//! specifications.
//!
//! - FLAC: optional leading ID3v2 (skipped), `"fLaC"`, the STREAMINFO block:
//!   sample rate 20 bits, channels-1 3 bits, bits-1 5 bits, total samples 36
//!   bits (0 = unknown).
//! - WAV: `RIFF`/`RF64` + `WAVE`, chunks 2-byte aligned; `fmt `: format tag
//!   (1 PCM, 3 float, 0xFFFE extensible: the real tag is the first 2 bytes
//!   of the sub-format GUID), channels, rate, block align, bits; `data` size
//!   / block align = frames (RF64: size from `ds64`).
//! - AIFF/AIFC: `FORM` + `AIFF`/`AIFC`, `COMM`: channels i16, frames u32,
//!   bits i16, rate as 80-bit IEEE extended; AIFC adds the compression type.
//!
//! All are case `Lossless`: no priming, exact length.
//!
//! ## 6. CAF
//!
//! Source: Apple Core Audio Format Specification 1.0. `caff`, version, then
//! chunks `type: [u8; 4], size: i64`. `desc`: sample rate f64, format id,
//! flags, bytes per packet, frames per packet, channels, bits. `pakt`:
//! packets i64, valid frames i64, priming frames i32, remainder frames i32.
//!
//! ## 7. Ogg
//!
//! Sources: RFC 3533 (Ogg pages), the Vorbis I specification (identification
//! header; granule positions, A.2), RFC 7845 (Ogg Opus, OpusHead, pre-skip).
//!
//! ### 7.1 Vorbis
//! First packet `0x01 "vorbis"`: version u32, channels u8, rate u32. The last
//! page's granule position is the stream length in samples. A first audio
//! page whose granule is less than the samples it decodes to would signal a
//! start trim; that is reported (`ogg_first_granule`) but not applied yet,
//! since encoders in practice start at 0.
//!
//! ### 7.2 Opus
//! `"OpusHead"`: version, channels, pre-skip u16 (48 kHz samples), input
//! rate u32. Decoding Opus is out of scope for now and returns
//! `Unsupported::Opus`.
//!
//! ## 8. Robustness requirements
//!
//! - The probe never panics on any byte input; all reads are bounds-checked.
//! - No allocation is sized from an unchecked header field: boxes and chunks
//!   are skipped by seeking, and the few that are read into memory are
//!   capped (64 KiB for `stsd`/`esds`/`ilst` items, 1 MiB for the MP3 sync
//!   window).
//! - Walks are bounded (nesting depth 8, 4096 boxes or chunks).
