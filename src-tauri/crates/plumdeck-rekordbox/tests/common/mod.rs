//! Synthetic ANLZ files built from the Deep Symmetry layout descriptions.
#![allow(dead_code)]

pub fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}

pub fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// `fourcc`, `len_header`, `len_tag`, then `body` (which starts at 0x0c).
pub fn tag(fourcc: &[u8; 4], len_header: u32, body: &[u8]) -> Vec<u8> {
    let mut out = fourcc.to_vec();
    out.extend(be32(len_header));
    out.extend(be32((12 + body.len()) as u32));
    out.extend(body);
    out
}

/// `PMAI` with a 0x1c-byte header followed by `tags`.
pub fn file(tags: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = tags.concat();
    let mut out = b"PMAI".to_vec();
    out.extend(be32(0x1c));
    out.extend(be32((0x1c + body.len()) as u32));
    out.extend([0u8; 16]);
    out.extend(body);
    out
}

pub fn utf16z(text: &str) -> Vec<u8> {
    let mut out: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    out.extend([0, 0]);
    out
}

/// PQTZ with `(beat_number, tempo_x100, time_ms)` entries.
pub fn pqtz(beats: &[(u16, u16, u32)]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(be32(0));
    body.extend(be32(0x0080_0000));
    body.extend(be32(beats.len() as u32));
    for &(n, t, ms) in beats {
        body.extend(be16(n));
        body.extend(be16(t));
        body.extend(be32(ms));
    }
    tag(b"PQTZ", 0x18, &body)
}

/// PQT2 with two edge beats and `entries` 2-byte entries.
pub fn pqt2(first: (u16, u16, u32), last: (u16, u16, u32), entries: &[[u8; 2]]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(be32(0));
    body.extend(be32(0x0100_0002));
    body.extend(be32(0));
    for (n, t, ms) in [first, last] {
        body.extend(be16(n));
        body.extend(be16(t));
        body.extend(be32(ms));
    }
    body.extend(be32(entries.len() as u32));
    body.extend([0u8; 12]);
    for e in entries {
        body.extend(e);
    }
    tag(b"PQT2", 0x38, &body)
}

/// One PCPT entry (0x38 bytes).
pub fn pcpt(hot_cue: u32, status: u32, cue_type: u8, time: u32, loop_time: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(be32(hot_cue));
    body.extend(be32(status));
    body.extend(be32(0x0001_0000));
    body.extend(be16(0xffff));
    body.extend(be16(1));
    body.push(cue_type);
    body.extend([0x00, 0x03, 0xe8]);
    body.extend(be32(time));
    body.extend(be32(loop_time));
    body.extend([0u8; 16]);
    tag(b"PCPT", 0x1c, &body)
}

pub fn pcob(list_type: u32, entries: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(be32(list_type));
    body.extend(be16(0));
    body.extend(be16(entries.len() as u16));
    body.extend(be32(0xffff_ffff));
    body.extend(entries.concat());
    tag(b"PCOB", 0x18, &body)
}

/// One PCP2 entry; `cut` truncates it to that many bytes (incomplete
/// entries exist in the wild) while patching `len_entry` to match.
pub struct Pcp2<'a> {
    pub hot_cue: u32,
    pub cue_type: u8,
    pub time: u32,
    pub loop_time: u32,
    pub color_id: u8,
    pub loop_num: u16,
    pub loop_den: u16,
    pub comment: Option<&'a str>,
    pub color: [u8; 4],
    pub cut: Option<usize>,
}

pub fn pcp2(e: &Pcp2<'_>) -> Vec<u8> {
    let comment = e.comment.map(utf16z).unwrap_or_default();
    let mut body = Vec::new();
    body.extend(be32(e.hot_cue));
    body.push(e.cue_type);
    body.extend([0x00, 0x03, 0xe8]);
    body.extend(be32(e.time));
    body.extend(be32(e.loop_time));
    body.push(e.color_id);
    body.extend([1, 0, 0, 0, 0, 0, 0]);
    body.extend(be16(e.loop_num));
    body.extend(be16(e.loop_den));
    body.extend(be32(comment.len() as u32));
    body.extend(&comment);
    body.extend(e.color);
    body.extend([0u8; 20]);
    let mut out = tag(b"PCP2", 0x10, &body);
    if let Some(cut) = e.cut {
        out.truncate(cut);
        out[8..12].copy_from_slice(&be32(cut as u32));
    }
    out
}

pub fn pco2(list_type: u32, entries: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(be32(list_type));
    body.extend(be16(entries.len() as u16));
    body.extend(be16(0));
    body.extend(entries.concat());
    tag(b"PCO2", 0x14, &body)
}

pub fn ppth(path: &str) -> Vec<u8> {
    let encoded = utf16z(path);
    let mut body = be32(encoded.len() as u32).to_vec();
    body.extend(encoded);
    tag(b"PPTH", 0x10, &body)
}

pub fn pvbr(index: &[u32]) -> Vec<u8> {
    let mut body = be32(0).to_vec();
    for &v in index {
        body.extend(be32(v));
    }
    tag(b"PVBR", 0x10, &body)
}

/// PWAV / PWV2.
pub fn preview(fourcc: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = be32(data.len() as u32).to_vec();
    body.extend(be32(0x0010_0000));
    body.extend(data);
    tag(fourcc, 0x14, &body)
}

/// PWV3/4/5/7 (with the unknown u32) or PWV6 (`unknown: None`).
pub fn table(fourcc: &[u8; 4], entry_bytes: u32, data: &[u8], unknown: Option<u32>) -> Vec<u8> {
    let mut body = be32(entry_bytes).to_vec();
    body.extend(be32(data.len() as u32 / entry_bytes));
    let len_header = match unknown {
        Some(u) => {
            body.extend(be32(u));
            0x18
        }
        None => 0x14,
    };
    body.extend(data);
    tag(fourcc, len_header, &body)
}

pub fn pwvc(unknown: u16, values: [u16; 3]) -> Vec<u8> {
    let mut body = be16(unknown).to_vec();
    for v in values {
        body.extend(be16(v));
    }
    tag(b"PWVC", 0x0e, &body)
}

/// One PSSI entry, fields as in the Deep Symmetry diagram.
#[derive(Clone, Copy, Default)]
pub struct Entry {
    pub index: u16,
    pub beat: u16,
    pub kind: u16,
    pub k1: u8,
    pub k2: u8,
    pub b: u8,
    pub beat2: u16,
    pub beat3: u16,
    pub beat4: u16,
    pub k3: u8,
    pub fill: u8,
    pub beat_fill: u16,
}

pub fn pssi(mood: u16, end_beat: u16, bank: u8, entries: &[Entry]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend(be32(24));
    body.extend(be16(entries.len() as u16));
    body.extend(be16(mood));
    body.extend([0u8; 6]);
    body.extend(be16(end_beat));
    body.extend([0u8; 2]);
    body.push(bank);
    body.push(0);
    for e in entries {
        body.extend(be16(e.index));
        body.extend(be16(e.beat));
        body.extend(be16(e.kind));
        body.extend([0, e.k1, 0, e.k2, 0, e.b]);
        body.extend(be16(e.beat2));
        body.extend(be16(e.beat3));
        body.extend(be16(e.beat4));
        body.extend([0, e.k3, 0, e.fill]);
        body.extend(be16(e.beat_fill));
    }
    tag(b"PSSI", 0x20, &body)
}

/// A `.DAT`-like file: PPTH, PVBR, PQTZ, two PCOB, PWAV, PWV2.
pub fn sample_dat() -> Vec<u8> {
    let beats: Vec<(u16, u16, u32)> = (0..64u32).map(|i| ((i % 4) as u16 + 1, 12800, 120 + i * 469)).collect();
    file(&[
        ppth("/Music/Caf\u{e9} Track.mp3"),
        pvbr(&(0..400).collect::<Vec<u32>>()),
        pqtz(&beats),
        pcob(1, &[pcpt(1, 0, 1, 1000, 0xffff_ffff), pcpt(2, 4, 2, 2000, 4000)]),
        pcob(0, &[]),
        preview(b"PWAV", &(0..400).map(|i| (i % 256) as u8).collect::<Vec<u8>>()),
        preview(b"PWV2", &[0x0f; 100]),
    ])
}

/// A `.EXT`-like file: PQT2, PCO2 lists, PWV3/4/5, PSSI.
pub fn sample_ext() -> Vec<u8> {
    let full = Pcp2 {
        hot_cue: 1,
        cue_type: 1,
        time: 1000,
        loop_time: 0xffff_ffff,
        color_id: 0,
        loop_num: 0,
        loop_den: 0,
        comment: Some("Drop"),
        color: [0x2a, 0xff, 0x80, 0x00],
        cut: None,
    };
    let looped = Pcp2 { hot_cue: 0, cue_type: 2, time: 2000, loop_time: 4000, color_id: 3, loop_num: 1, loop_den: 2, comment: None, color: [0; 4], cut: None };
    file(&[
        pqt2((1, 12800, 120), (4, 12800, 29667), &[[1, 0], [2, 0]]),
        pco2(1, &[pcp2(&full)]),
        pco2(0, &[pcp2(&looped)]),
        table(b"PWV3", 1, &[0xe5; 300], Some(0x0096_0000)),
        table(b"PWV4", 6, &[7u8; 7200], Some(0)),
        table(b"PWV5", 2, &[0xff, 0xfc, 0x00, 0x05], Some(0x0096_0305)),
        pssi(
            1,
            64,
            0,
            &[
                Entry { index: 1, beat: 1, kind: 1, k1: 1, ..Entry::default() },
                Entry { index: 2, beat: 17, kind: 2, k2: 1, b: 1, beat2: 20, beat3: 24, beat4: 28, ..Entry::default() },
                Entry { index: 3, beat: 33, kind: 5, k1: 0, fill: 1, beat_fill: 45, ..Entry::default() },
            ],
        ),
    ])
}

/// A `.2EX`-like file: PWV6, PWV7, PWVC, plus an unknown tag.
pub fn sample_2ex() -> Vec<u8> {
    let preview: Vec<u8> = (0..1200 * 3).map(|i| (i % 128) as u8).collect();
    file(&[
        table(b"PWV6", 3, &preview, None),
        table(b"PWV7", 3, &[10, 20, 30, 40, 50, 60], Some(0x0096_0000)),
        pwvc(0, [0x60, 0x70, 0x7f]),
        tag(b"PZZZ", 0x10, &[1, 2, 3, 4]),
    ])
}
