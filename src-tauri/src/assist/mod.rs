//! Assist mode: read-only observation of the decks rekordbox currently has loaded.
//!
//! Everything here is observation only. We never send events to rekordbox, never
//! touch its database or settings, and never capture the screen — the deck state
//! comes from the macOS Accessibility tree of rekordbox's own window, and the
//! candidate file paths come from the files that process already has open.

use serde::Serialize;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
#[path = "macos/decks.rs"]
mod decks;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::AssistState;

pub mod commands;

/// One deck slot as rekordbox is currently drawing it.
///
/// Every field is "what the UI says", not "what we believe is true": resolution
/// against the library happens in the backend using the exact file path.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct DeckObservation {
    /// Deck number as printed next to the deck (1..4).
    pub slot: u32,
    pub loaded: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    /// The BPM rekordbox prints in the deck header (its stored track BPM).
    pub track_bpm: Option<f64>,
    /// The tempo readout on the platter (stored BPM with the pitch fader applied).
    pub tempo_bpm: Option<f64>,
    /// Key exactly as displayed (classical or Camelot, depending on the user's setting).
    pub display_key: Option<String>,
    /// The elapsed-time readout in the deck header, in seconds. rekordbox exposes
    /// no play state, so the UI infers "playing" from this advancing between
    /// snapshots. It changes every tick, so it must never feed `signature`.
    pub elapsed_seconds: Option<f64>,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct AssistSnapshot {
    /// false on platforms where we have no deck-reading implementation at all.
    pub supported: bool,
    pub permission_granted: bool,
    pub app_running: bool,
    pub app_path: Option<String>,
    /// Raw deck-layout selector text, shown as-is. Never used to invent decks.
    pub layout_hint: Option<String>,
    pub decks: Vec<DeckObservation>,
    /// Audio files the rekordbox process currently has open. This includes the
    /// sampler and the metronome click, so it is a candidate set, not deck state.
    pub open_audio_paths: Vec<String>,
    pub open_paths_error: Option<String>,
    /// Changes whenever the visible deck contents change; drives refresh.
    pub signature: String,
    pub captured_at_ms: u64,
    /// Set when `decks` could not be read; rendered verbatim in the UI.
    pub unavailable_reason: Option<String>,
    pub warnings: Vec<String>,
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl AssistSnapshot {
    pub fn unsupported(reason: &str) -> Self {
        Self {
            supported: false,
            unavailable_reason: Some(reason.to_string()),
            ..Default::default()
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos::AssistState;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Default)]
pub struct AssistState;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl AssistState {
    pub fn snapshot(&self) -> AssistSnapshot {
        AssistSnapshot::unsupported("デッキ読み取りは macOS と Windows に対応しています")
    }

    pub fn request_permission(&self) -> bool {
        false
    }

    pub fn open_permission_settings(&self) -> Result<(), String> {
        Err("このプラットフォームには該当する設定画面がありません".into())
    }
}
