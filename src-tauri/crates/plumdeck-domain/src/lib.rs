//! Pure domain rules shared by the repositories and services (plan §1.2:
//! core → domain → db). Nothing here does I/O.
//!
//! - [`audio_math`]: the library's key filter and sort
//!   (backend/infra/repositories/track_repository.py) over plumdeck-key's
//!   `normalize_key`.
//! - [`genre_rules`]: label clean-up for MCP genre analyses and the
//!   spelling-variant grouping of `GET /api/genres/cleanup-suggestions`
//!   (backend/app/services/genre_app_service.py).
//! - [`lrc`]: LRC time tags in the lyrics search (backend/api/routers/lyrics.py).
//! - [`mixability`], [`targets`], [`setlist_builder`], [`set_duration`],
//!   [`wordplay`]: the recommenders, Setlist Auto and path builder, set
//!   timing and wordplay rules (S4).
//! - [`performance`]: performance metadata models, playback grid fit and
//!   rekordbox grid shift (S5).

pub mod audio_math;
pub mod genre_rules;
pub mod lrc;
pub mod mixability;
pub mod performance;
pub mod pychars;
pub mod set_duration;
pub mod setlist_builder;
pub mod targets;
pub mod wordplay;
