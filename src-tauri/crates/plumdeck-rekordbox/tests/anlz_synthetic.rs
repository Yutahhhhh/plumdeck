//! ANLZ parsing against synthetic files built from the published layout, plus
//! fuzz-style robustness checks (truncation, bit flips, hostile lengths).

mod common;

use common::*;
use plumdeck_rekordbox::anlz::phrase::mask_section;
use plumdeck_rekordbox::anlz::*;

#[test]
fn dat_tags_parse() {
    let file = AnlzFile::parse(&sample_dat()).unwrap();
    assert!(file.issues.is_empty(), "{:?}", file.issues);
    assert_eq!(file.invalid_tags().count(), 0);
    assert_eq!(file.path(), Some("/Music/Caf\u{e9} Track.mp3"));
    let vbr = file.vbr().unwrap();
    assert_eq!(vbr.entries.len(), 400);
    assert_eq!(vbr.entries[399], 399);

    let grid = file.beat_grid().unwrap();
    assert_eq!(grid.beats.len(), 64);
    assert_eq!(grid.beats[0], Beat { beat_number: 1, tempo_x100: 12800, time_ms: 120 });
    assert_eq!(grid.beats[5].beat_number, 2);
    assert_eq!(grid.median_bpm(), Some(128.0));
    assert_eq!(grid.time_of_beat(1), Some(120));
    assert_eq!(grid.time_of_beat(0), None);
    assert_eq!(grid.time_of_beat(65), None);

    let lists: Vec<&CueList> = file.cue_lists().collect();
    assert_eq!(lists.len(), 2);
    assert_eq!(lists[0].list_type, CueListType::HotCue);
    assert_eq!(lists[0].entries.len(), 2);
    assert!(!lists[0].truncated);
    let looped = &lists[0].entries[1];
    assert_eq!((looped.hot_cue, looped.status, looped.cue_type), (2, 4, 2));
    assert_eq!((looped.time_ms, looped.loop_time_ms), (2000, 4000));
    assert_eq!(lists[0].entries[0].order_first, 0xffff);
    assert_eq!(lists[1].list_type, CueListType::Memory);
    assert!(lists[1].entries.is_empty());

    let pwav = file.waveform(WaveformKind::Preview).unwrap();
    assert_eq!(pwav.columns(), 400);
    let col = pwav.mono().nth(0xe5).unwrap();
    assert_eq!((col.height, col.whiteness), (0x05, 0x7));
    let tiny = file.waveform(WaveformKind::TinyPreview).unwrap();
    assert_eq!(tiny.columns(), 100);
    assert!(tiny.mono().all(|c| c.height == 15 && c.whiteness == 0));
    assert_eq!(tiny.three_band().count(), 0, "typed accessors ignore other kinds");
}

#[test]
fn ext_tags_parse() {
    let file = AnlzFile::parse(&sample_ext()).unwrap();
    assert!(file.issues.is_empty());
    assert_eq!(file.invalid_tags().count(), 0);

    let pqt2 = file.extended_beat_grid().unwrap();
    assert_eq!(pqt2.edge_beats[1].time_ms, 29667);
    assert_eq!(pqt2.entries, vec![[1, 0], [2, 0]]);

    let lists: Vec<&ExtendedCueList> = file.extended_cue_lists().collect();
    assert_eq!(lists.len(), 2);
    let hot = &lists[0].entries[0];
    assert_eq!(hot.hot_cue, 1);
    assert_eq!(hot.comment.as_deref(), Some("Drop"));
    assert_eq!(hot.color_code, Some(0x2a));
    assert_eq!(hot.color_rgb, Some([0xff, 0x80, 0x00]));
    let mem = &lists[1].entries[0];
    assert_eq!((mem.cue_type, mem.time_ms, mem.loop_time_ms), (2, 2000, 4000));
    assert_eq!((mem.color_id, mem.loop_numerator, mem.loop_denominator), (Some(3), Some(1), Some(2)));
    assert_eq!(mem.comment, None);
    assert_eq!(mem.color_code, Some(0));

    let pwv3 = file.waveform(WaveformKind::Detail).unwrap();
    assert_eq!((pwv3.columns(), pwv3.unknown), (300, 0x0096_0000));
    assert_eq!(file.waveform(WaveformKind::ColorPreview).unwrap().color_preview().count(), 1200);
    let pwv5: Vec<ColorColumn> = file.waveform(WaveformKind::ColorDetail).unwrap().color_detail().collect();
    assert_eq!(pwv5[0], ColorColumn { red: 7, green: 7, blue: 7, height: 31, low_bits: 0 });
    assert_eq!(pwv5[1], ColorColumn { red: 0, green: 0, blue: 0, height: 1, low_bits: 1 });

    let pssi = file.song_structure().unwrap();
    assert_eq!(pssi.mood, Mood::High);
    assert!(!pssi.masked);
    assert_eq!(pssi.end_beat, 64);
    assert_eq!(pssi.bank(), Some(Bank::Default));
    let labels: Vec<_> = pssi.phrases.iter().map(|p| pssi.label(p).unwrap()).collect();
    assert_eq!(labels, ["Intro 1", "Up 3", "Chorus 2"]);
    let up = &pssi.phrases[1];
    assert_eq!((up.b, up.beat2, up.beat3, up.beat4), (1, 20, 24, 28));
    assert!(pssi.phrases[2].has_fill());
    assert_eq!(pssi.phrases[2].beat_fill, 45);
}

#[test]
fn two_ex_tags_parse_and_unknown_is_skipped() {
    let file = AnlzFile::parse(&sample_2ex()).unwrap();
    let pwv6 = file.waveform(WaveformKind::ThreeBandPreview).unwrap();
    assert_eq!(pwv6.columns(), 1200);
    let first = pwv6.three_band().next().unwrap();
    assert_eq!((first.low, first.mid, first.high), (0, 1, 2));
    let pwv7: Vec<ThreeBandColumn> = file.waveform(WaveformKind::ThreeBandDetail).unwrap().three_band().collect();
    assert_eq!(pwv7, [ThreeBandColumn { low: 10, mid: 20, high: 30 }, ThreeBandColumn { low: 40, mid: 50, high: 60 }]);
    assert_eq!(file.waveform_calibration().unwrap().values, [0x60, 0x70, 0x7f]);
    assert!(matches!(file.tags.last(), Some(Tag::Unknown { fourcc, len: 16 }) if fourcc == b"PZZZ"));
}

#[test]
fn masked_pssi_is_detected_and_unmasked() {
    let entries = [
        Entry { index: 1, beat: 1, kind: 1, ..Entry::default() },
        Entry { index: 2, beat: 33, kind: 8, fill: 1, beat_fill: 61, ..Entry::default() },
    ];
    let plain = pssi(2, 64, 7, &entries);
    let masked = mask_section(&plain);
    assert_ne!(plain, masked);
    let a = AnlzFile::parse(&file(&[plain])).unwrap();
    let b = AnlzFile::parse(&file(&[masked])).unwrap();
    let (a, b) = (a.song_structure().unwrap(), b.song_structure().unwrap());
    assert!(!a.masked && b.masked);
    assert_eq!(a.phrases, b.phrases);
    assert_eq!((b.mood, b.end_beat, b.bank()), (Mood::Mid, 64, Some(Bank::Club1)));
    assert_eq!(b.label(&b.phrases[1]), Some("Bridge"));

    // Neither raw nor unmasked mood valid: kept raw, flagged unknown.
    let odd = pssi(77, 1, 0, &entries);
    let parsed = AnlzFile::parse(&file(&[odd])).unwrap();
    let s = parsed.song_structure().unwrap();
    assert_eq!(s.mood, Mood::Unknown(77));
    assert!(!s.masked);
}

#[test]
fn incomplete_pco2_entries_keep_what_they_have() {
    let base = Pcp2 {
        hot_cue: 2,
        cue_type: 1,
        time: 500,
        loop_time: 0,
        color_id: 0,
        loop_num: 0,
        loop_den: 0,
        comment: Some("Vox"),
        color: [1, 2, 3, 4],
        cut: None,
    };
    let comment_len = 8; // "Vox" + NUL in UTF-16
    // Ends right after loop_time: no colour id, comment or colour.
    let short = pcp2(&Pcp2 { cut: Some(0x1c), ..base });
    // Ends after the comment: no hot cue colour.
    let no_color = pcp2(&Pcp2 { cut: Some(0x2c + comment_len), ..base });
    let full = pcp2(&base);
    let parsed = AnlzFile::parse(&file(&[pco2(1, &[short, no_color, full])])).unwrap();
    let list = parsed.extended_cue_lists().next().unwrap();
    assert!(!list.truncated);
    let e = &list.entries;
    assert_eq!((e[0].time_ms, e[0].color_id, e[0].comment.as_deref(), e[0].color_code), (500, None, None, None));
    assert_eq!((e[1].comment.as_deref(), e[1].color_code), (Some("Vox"), None));
    assert_eq!((e[2].comment.as_deref(), e[2].color_code, e[2].color_rgb), (Some("Vox"), Some(1), Some([2, 3, 4])));
}

#[test]
fn hostile_counts_invalidate_only_their_tag() {
    // PQTZ claims more beats than it holds.
    let mut grid = pqtz(&[(1, 12000, 0), (2, 12000, 500)]);
    grid[0x14..0x18].copy_from_slice(&be32(u32::MAX));
    // PWV7 with a wrong entry size.
    let mut pwv7 = table(b"PWV7", 3, &[1, 2, 3], Some(0));
    pwv7[0x0c..0x10].copy_from_slice(&be32(4));
    // PPTH whose length runs past the section.
    let mut path = ppth("x");
    path[0x0c..0x10].copy_from_slice(&be32(1 << 30));
    // PCOB announcing 3 entries but holding 1.
    let mut cues = pcob(1, &[pcpt(1, 0, 1, 10, 0)]);
    cues[0x12..0x14].copy_from_slice(&be16(3));
    let parsed = AnlzFile::parse(&file(&[grid, pwv7, path, cues, ppth("ok")])).unwrap();
    let invalid: Vec<_> = parsed.invalid_tags().map(|(f, _)| f.to_owned()).collect();
    assert_eq!(invalid, ["PQTZ", "PWV7", "PPTH"]);
    let list = parsed.cue_lists().next().unwrap();
    assert!(list.truncated);
    assert_eq!(list.entries.len(), 1);
    assert_eq!(parsed.path(), Some("ok"));
}

#[test]
fn bad_headers_are_errors_and_bad_sections_stop_the_walk() {
    assert_eq!(AnlzFile::parse(b""), Err(AnlzError::NotAnlz));
    assert_eq!(AnlzFile::parse(b"PMAX\0\0\0\x1c"), Err(AnlzError::NotAnlz));
    assert_eq!(AnlzFile::parse(b"PMAI\0\0\0"), Err(AnlzError::BadHeader));
    assert_eq!(AnlzFile::parse(b"PMAI\0\0\0\x04\0\0\0\x0c"), Err(AnlzError::BadHeader));
    assert_eq!(AnlzFile::parse(b"PMAI\xff\xff\xff\xff\0\0\0\x0c"), Err(AnlzError::BadHeader));

    let mut data = sample_dat();
    // Corrupt the second section's len_tag to point past the end.
    let second = 0x1c + u32::from_be_bytes(data[0x1c + 8..0x1c + 12].try_into().unwrap()) as usize;
    data[second + 8..second + 12].copy_from_slice(&be32(0x7fff_ffff));
    let parsed = AnlzFile::parse(&data).unwrap();
    assert_eq!(parsed.tags.len(), 1, "the PPTH before the damage survives");
    assert_eq!(parsed.issues, [Issue::BrokenSection { offset: second }]);

    // Declared file length differs from the bytes present.
    let mut data = sample_2ex();
    data.extend([0u8; 7]);
    let parsed = AnlzFile::parse(&data).unwrap();
    assert!(matches!(parsed.issues[0], Issue::FileLengthMismatch { .. }));
    assert!(parsed.waveform(WaveformKind::ThreeBandDetail).is_some());

    // Zero-length section: must not loop forever.
    let mut data = file(&[]);
    data.extend(b"PQTZ\0\0\0\0\0\0\0\0");
    let len = data.len() as u32;
    data[8..12].copy_from_slice(&be32(len));
    let parsed = AnlzFile::parse(&data).unwrap();
    assert_eq!(parsed.issues.len(), 1);
}

#[test]
fn read_refuses_missing_and_oversized_files() {
    assert!(matches!(AnlzFile::read(std::path::Path::new("/nonexistent/ANLZ0000.DAT")), Err(AnlzError::Io(_))));
    let big = vec![0u8; (MAX_FILE_BYTES + 1) as usize];
    assert_eq!(AnlzFile::parse(&big), Err(AnlzError::TooLarge));
}

/// xorshift64*: deterministic, dependency-free randomness for the fuzz loops.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn exercise(data: &[u8]) {
    // Parsing must never panic; touch every accessor on success.
    if let Ok(file) = AnlzFile::parse(data) {
        let _ = file.beat_grid().map(|g| (g.median_bpm(), g.time_of_beat(u16::MAX)));
        let _ = file.extended_beat_grid();
        let _ = file.cue_lists().count() + file.extended_cue_lists().count();
        let _ = file.path();
        let _ = file.vbr();
        for kind in [
            WaveformKind::Preview,
            WaveformKind::TinyPreview,
            WaveformKind::Detail,
            WaveformKind::ColorPreview,
            WaveformKind::ColorDetail,
            WaveformKind::ThreeBandPreview,
            WaveformKind::ThreeBandDetail,
        ] {
            if let Some(w) = file.waveform(kind) {
                let _ = w.mono().count() + w.color_detail().count() + w.color_preview().count() + w.three_band().count();
            }
        }
        if let Some(s) = file.song_structure() {
            for p in &s.phrases {
                let _ = s.label(p);
            }
            let _ = s.bank();
        }
        let _ = file.invalid_tags().count();
    }
}

#[test]
fn every_truncation_is_safe() {
    for sample in [sample_dat(), sample_ext(), sample_2ex()] {
        for len in 0..=sample.len() {
            exercise(&sample[..len]);
            // Also with the declared length patched to the truncated size, so
            // the walker reaches the damaged section bodies.
            if len >= 12 {
                let mut cut = sample[..len].to_vec();
                cut[8..12].copy_from_slice(&be32(len as u32));
                exercise(&cut);
            }
        }
    }
}

#[test]
fn random_corruption_is_safe() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let samples = [sample_dat(), sample_ext(), sample_2ex()];
    for round in 0..6000 {
        let mut data = samples[round % samples.len()].clone();
        let edits = 1 + rng.below(8);
        for _ in 0..edits {
            let at = rng.below(data.len());
            match rng.below(4) {
                0 => data[at] ^= 1 << rng.below(8),
                1 => data[at] = rng.next() as u8,
                // Hostile 32-bit length/count at an aligned-ish position.
                2 if at + 4 <= data.len() => {
                    let value = [0u32, 1, 0xffff, 0x7fff_ffff, u32::MAX, rng.next() as u32][rng.below(6)];
                    data[at..at + 4].copy_from_slice(&be32(value));
                }
                _ => data.truncate(at.max(1)),
            }
        }
        exercise(&data);
    }
}

#[test]
fn random_bytes_after_a_valid_header_are_safe() {
    let mut rng = Rng(42);
    for _ in 0..2000 {
        let len = rng.below(600);
        let mut data = file(&[]);
        data.extend((0..len).map(|_| rng.next() as u8));
        // Seed some known fourccs so tag bodies get exercised.
        for fourcc in [b"PQTZ", b"PSSI", b"PCO2", b"PCOB", b"PWV7", b"PPTH"] {
            if data.len() > 0x1c + 4 && rng.below(3) == 0 {
                let at = 0x1c + rng.below(data.len() - 0x1c - 4);
                data[at..at + 4].copy_from_slice(fourcc);
            }
        }
        let total = data.len() as u32;
        data[8..12].copy_from_slice(&be32(total));
        exercise(&data);
    }
}

#[test]
fn random_bodies_inside_valid_envelopes_are_safe() {
    let fourccs: [&[u8; 4]; 15] = [
        b"PQTZ", b"PQT2", b"PCOB", b"PCO2", b"PPTH", b"PVBR", b"PWAV", b"PWV2", b"PWV3", b"PWV4", b"PWV5", b"PWV6",
        b"PWV7", b"PWVC", b"PSSI",
    ];
    let mut rng = Rng(7);
    for _ in 0..20000 {
        let tags: Vec<Vec<u8>> = (0..1 + rng.below(3))
            .map(|_| {
                let fourcc = fourccs[rng.below(fourccs.len())];
                let body: Vec<u8> = (0..rng.below(96))
                    .map(|_| match rng.below(4) {
                        0 => 0,
                        1 => 0xff,
                        _ => rng.next() as u8,
                    })
                    .collect();
                tag(fourcc, 12, &body)
            })
            .collect();
        exercise(&file(&tags));
    }
}

#[test]
fn ppth_matching() {
    assert!(ppth_matches("?/Caf\u{e9}.mp3", "/Music/x/Cafe\u{301}.mp3"));
    assert!(!ppth_matches("?/Other.mp3", "/Music/x/Cafe.mp3"));
    assert!(!ppth_matches("?/", "/Music/x/"));
    assert!(ppth_matches("/Contents/A/b.mp3", "/Volumes/USB/Contents/A/b.mp3"));
    assert!(ppth_matches("/Music/b.mp3", "/Music/b.mp3"));
    assert!(!ppth_matches("b.mp3", "/Music/ab.mp3"));
    assert!(ppth_matches("b.mp3", "/Music/b.mp3"));
    assert!(!ppth_matches("", "/Music/b.mp3"));
}
