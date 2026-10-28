//! PSSI, the song structure (phrase) section of `.EXT` files.
//!
//! Header: `len_entry_bytes` (0x0c), `len_entries` (0x10), `mood` (0x12),
//! `end_beat` (0x1a), `bank` (0x1e); 24-byte entries from 0x20.
//!
//! Exported files (USB) XOR-mask every byte after `len_entries` with
//! `BASE[i % 19] + len_entries`. Local library files are not masked. A tag is
//! treated as masked when its raw mood is not 1–3 but the unmasked mood is.

use super::bytes::{u16_at, u32_at, u8_at};

const HEADER: usize = 0x20;
const MASK_FROM: usize = 0x12;
const MASK_BASE: [u8; 19] =
    [0xCB, 0xE1, 0xEE, 0xFA, 0xE5, 0xEE, 0xAD, 0xEE, 0xE9, 0xD2, 0xE9, 0xEB, 0xE1, 0xE9, 0xF3, 0xE8, 0xE9, 0xF4, 0xE1];

/// The overall phrase vocabulary rekordbox picked for the track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mood {
    High,
    Mid,
    Low,
    Unknown(u16),
}

impl Mood {
    pub fn from_raw(value: u16) -> Self {
        match value {
            1 => Mood::High,
            2 => Mood::Mid,
            3 => Mood::Low,
            other => Mood::Unknown(other),
        }
    }

    pub fn raw(self) -> u16 {
        match self {
            Mood::High => 1,
            Mood::Mid => 2,
            Mood::Low => 3,
            Mood::Unknown(v) => v,
        }
    }
}

/// Lighting bank chosen in Lighting mode (`raw` keeps unexpected values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bank {
    Default,
    Cool,
    Natural,
    Hot,
    Subtle,
    Warm,
    Vivid,
    Club1,
    Club2,
}

impl Bank {
    pub fn from_raw(value: u8) -> Option<Self> {
        Some(match value {
            0 => Bank::Default,
            1 => Bank::Cool,
            2 => Bank::Natural,
            3 => Bank::Hot,
            4 => Bank::Subtle,
            5 => Bank::Warm,
            6 => Bank::Vivid,
            7 => Bank::Club1,
            8 => Bank::Club2,
            _ => return None,
        })
    }
}

/// One phrase entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phrase {
    pub index: u16,
    /// First beat, as a 1-based index into the PQTZ beats.
    pub beat: u16,
    pub kind: u16,
    pub k1: u8,
    pub k2: u8,
    pub k3: u8,
    /// 0: one extra beat in `beat2`; 1: three in `beat2..beat4` (Up 3).
    pub b: u8,
    pub beat2: u16,
    pub beat3: u16,
    pub beat4: u16,
    /// Non-zero when the phrase ends in a fill.
    pub fill: u8,
    pub beat_fill: u16,
}

impl Phrase {
    pub fn has_fill(&self) -> bool {
        self.fill != 0
    }
}

/// PSSI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SongStructure {
    pub mood: Mood,
    /// The beat (1-based) where the last phrase ends. Can exceed the number of
    /// PQTZ beats; clamp before converting to time.
    pub end_beat: u16,
    pub raw_bank: u8,
    /// Whether the section was XOR-masked (exported files).
    pub masked: bool,
    pub phrases: Vec<Phrase>,
    /// Fewer entries were present than `len_entries` announced.
    pub truncated: bool,
}

impl SongStructure {
    pub fn bank(&self) -> Option<Bank> {
        Bank::from_raw(self.raw_bank)
    }

    /// The label rekordbox shows for `phrase` under this track's mood.
    pub fn label(&self, phrase: &Phrase) -> Option<&'static str> {
        phrase_label(self.mood, phrase)
    }
}

/// Phrase labels (Deep Symmetry, "Phrase labels in each mood" and "High mood
/// phrase variants").
pub fn phrase_label(mood: Mood, p: &Phrase) -> Option<&'static str> {
    match mood {
        Mood::Low => Some(match p.kind {
            1 => "Intro",
            2..=4 => "Verse 1",
            5..=7 => "Verse 2",
            8 => "Bridge",
            9 => "Chorus",
            10 => "Outro",
            _ => return None,
        }),
        Mood::Mid => Some(match p.kind {
            1 => "Intro",
            2 => "Verse 1",
            3 => "Verse 2",
            4 => "Verse 3",
            5 => "Verse 4",
            6 => "Verse 5",
            7 => "Verse 6",
            8 => "Bridge",
            9 => "Chorus",
            10 => "Outro",
            _ => return None,
        }),
        Mood::High => Some(match (p.kind, p.k1, p.k2, p.k3) {
            (1, 1, _, _) => "Intro 1",
            (1, _, _, _) => "Intro 2",
            (2, _, 0, 0) => "Up 1",
            (2, _, 0, 1) => "Up 2",
            (2, _, 1, 0) => "Up 3",
            (3, _, _, _) => "Down",
            (5, 1, _, _) => "Chorus 1",
            (5, _, _, _) => "Chorus 2",
            (6, 1, _, _) => "Outro 1",
            (6, _, _, _) => "Outro 2",
            _ => return None,
        }),
        Mood::Unknown(_) => None,
    }
}

fn unmask(tag: &[u8], len_entries: u16) -> Vec<u8> {
    let add = len_entries as u8; // the mask adds len_entries modulo 256
    tag.iter()
        .enumerate()
        .map(|(i, &byte)| match i.checked_sub(MASK_FROM) {
            Some(j) => byte ^ MASK_BASE[j % MASK_BASE.len()].wrapping_add(add),
            None => byte,
        })
        .collect()
}

fn valid_mood(value: Option<u16>) -> bool {
    matches!(value, Some(1..=3))
}

pub(super) fn parse_pssi(tag: &[u8]) -> Result<SongStructure, &'static str> {
    let entry_bytes = u32_at(tag, 0x0c).ok_or("truncated header")? as usize;
    if entry_bytes < 24 {
        return Err("unexpected entry size");
    }
    let len_entries = u16_at(tag, 0x10).ok_or("truncated header")?;
    if tag.len() < HEADER {
        return Err("truncated header");
    }
    let raw_mood = u16_at(tag, MASK_FROM);
    let unmasked;
    let (data, masked): (&[u8], bool) = if valid_mood(raw_mood) {
        (tag, false)
    } else {
        unmasked = unmask(tag, len_entries);
        if valid_mood(u16_at(&unmasked, MASK_FROM)) {
            (&unmasked, true)
        } else {
            (tag, false)
        }
    };
    let mood = Mood::from_raw(u16_at(data, MASK_FROM).ok_or("truncated header")?);
    let end_beat = u16_at(data, 0x1a).ok_or("truncated header")?;
    let raw_bank = u8_at(data, 0x1e).ok_or("truncated header")?;
    let available = (data.len() - HEADER) / entry_bytes;
    let count = usize::from(len_entries).min(available);
    let mut phrases = Vec::with_capacity(count);
    for i in 0..count {
        let at = HEADER + i * entry_bytes;
        let get8 = |off: usize| u8_at(data, at + off).unwrap_or(0);
        let get16 = |off: usize| u16_at(data, at + off).unwrap_or(0);
        phrases.push(Phrase {
            index: get16(0x00),
            beat: get16(0x02),
            kind: get16(0x04),
            k1: get8(0x07),
            k2: get8(0x09),
            b: get8(0x0b),
            beat2: get16(0x0c),
            beat3: get16(0x0e),
            beat4: get16(0x10),
            k3: get8(0x13),
            fill: get8(0x15),
            beat_fill: get16(0x16),
        });
    }
    Ok(SongStructure {
        mood,
        end_beat,
        raw_bank,
        masked,
        truncated: count < usize::from(len_entries),
        phrases,
    })
}

/// Applies the export mask to a plain PSSI section (test helper: the mask is
/// its own inverse).
#[doc(hidden)]
pub fn mask_section(tag: &[u8]) -> Vec<u8> {
    let len_entries = u16_at(tag, 0x10).unwrap_or(0);
    unmask(tag, len_entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phrase(kind: u16, k1: u8, k2: u8, k3: u8) -> Phrase {
        Phrase { index: 1, beat: 1, kind, k1, k2, k3, b: 0, beat2: 0, beat3: 0, beat4: 0, fill: 0, beat_fill: 0 }
    }

    #[test]
    fn labels_follow_the_tables() {
        assert_eq!(phrase_label(Mood::High, &phrase(1, 1, 0, 0)), Some("Intro 1"));
        assert_eq!(phrase_label(Mood::High, &phrase(1, 0, 0, 0)), Some("Intro 2"));
        assert_eq!(phrase_label(Mood::High, &phrase(2, 0, 0, 1)), Some("Up 2"));
        assert_eq!(phrase_label(Mood::High, &phrase(2, 0, 1, 0)), Some("Up 3"));
        assert_eq!(phrase_label(Mood::High, &phrase(2, 0, 1, 1)), None);
        assert_eq!(phrase_label(Mood::High, &phrase(4, 0, 0, 0)), None);
        assert_eq!(phrase_label(Mood::Mid, &phrase(7, 0, 0, 0)), Some("Verse 6"));
        assert_eq!(phrase_label(Mood::Low, &phrase(4, 0, 0, 0)), Some("Verse 1"));
        assert_eq!(phrase_label(Mood::Low, &phrase(11, 0, 0, 0)), None);
        assert_eq!(phrase_label(Mood::Unknown(9), &phrase(1, 0, 0, 0)), None);
    }

    #[test]
    fn bank_values() {
        assert_eq!(Bank::from_raw(8), Some(Bank::Club2));
        assert_eq!(Bank::from_raw(0xf6), None);
    }
}
