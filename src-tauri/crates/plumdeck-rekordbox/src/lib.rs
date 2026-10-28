//! Read-only access to a local rekordbox library.
//!
//! - [`masterdb`]: the SQLCipher-encrypted `master.db`, opened read-only and
//!   `query_only`, with typed accessors for the tables plumdeck reads.
//! - [`paths`]: file identity helpers shared with the Python backend
//!   (`infra/rekordbox_library.py`).
//! - [`locate`]: where `master.db` and the analysis files live.
//!
//! Nothing in this crate writes to rekordbox's files.

#![forbid(unsafe_code)]

pub mod locate;
pub mod masterdb;
pub mod paths;

pub use masterdb::{MasterDb, RekordboxError};
