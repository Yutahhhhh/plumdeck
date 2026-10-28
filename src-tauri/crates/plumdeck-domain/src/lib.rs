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

pub mod audio_math;
pub mod genre_rules;
pub mod lrc;
pub mod pychars;
