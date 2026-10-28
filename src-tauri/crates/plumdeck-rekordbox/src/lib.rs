//! Read-only access to a local rekordbox library.
//!
//! - [`masterdb`]: the SQLCipher-encrypted `master.db`, opened read-only and
//!   `query_only`, with typed accessors for the tables plumdeck reads.
//! - [`anlz`]: a bounds-checked parser for the `.DAT` / `.EXT` / `.2EX`
//!   analysis files (beat grid, cues, waveforms, phrases).
//! - [`paths`]: file identity helpers shared with the Python backend
//!   (`infra/rekordbox_library.py`).
//! - [`locate`]: where `master.db` and the analysis files live.
//!
//! Nothing in this crate writes to rekordbox's files. The ANLZ parser was
//! written from Deep Symmetry's public format description
//! (<https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html>)
//! and checked against real files; no GPL code was consulted.

#![forbid(unsafe_code)]

pub mod anlz;
pub mod locate;
pub mod masterdb;
pub mod paths;

pub use masterdb::{MasterDb, RekordboxError};
