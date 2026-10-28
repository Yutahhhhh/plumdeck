//! Repositories over a leased connection: what startup touches (S1), the
//! settings and dashboard statistics (S2), the library's tracks, lyrics and
//! genres (S3).

pub mod analyzed;
pub mod dashboard;
pub mod genres;
pub mod imports;
pub mod lyrics;
pub mod settings;
pub mod tracks;
