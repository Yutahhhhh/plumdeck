//! Repositories over a leased connection: what startup touches (S1), the
//! settings and dashboard statistics (S2), the library's tracks, lyrics and
//! genres (S3), setlists, recommendations and wordplay pairs (S4).

pub mod analyzed;
pub mod dashboard;
pub mod genres;
pub mod imports;
pub mod lyrics;
pub mod performance;
pub mod recommendations;
pub mod settings;
pub mod setlists;
pub mod tracks;
pub mod wordplay;
