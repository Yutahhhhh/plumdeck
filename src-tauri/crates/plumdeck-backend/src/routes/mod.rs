//! HTTP routes, one module per Python router.

pub mod dashboard;
pub mod genres;
pub mod lyrics;
pub mod media;
pub mod settings;
pub mod tracks;

use axum::routing::{get, patch, post};
use axum::Router;

use crate::AppState;

/// Every ported `/api` route.
pub fn api() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(settings::get_settings).post(settings::update_setting))
        .route("/api/dashboard", get(dashboard::get_dashboard))
        .route("/api/tracks", get(tracks::get_tracks))
        .route("/api/tracks/page", get(tracks::get_tracks_page))
        .route("/api/tracks/count", get(tracks::get_tracks_count))
        .route("/api/tracks/ids", get(tracks::get_track_ids))
        .route("/api/tracks/{track_id}/info", patch(tracks::update_info))
        .route("/api/tracks/{track_id}/genre", patch(tracks::update_track_genre))
        .route("/api/tracks/{track_id}/lyrics", get(lyrics::get_track_lyrics).put(lyrics::put_track_lyrics))
        .route("/api/lyrics/search", get(lyrics::search))
        .route("/api/genres/list", get(genres::list_genres))
        .route("/api/genres/subgenres", get(genres::list_subgenres))
        .route("/api/genres/unknown", get(genres::unknown))
        .route("/api/genres/unknown-ids", get(genres::unknown_ids))
        .route("/api/genres/cleanup-suggestions", get(genres::cleanup_suggestions))
        .route("/api/genres/batch-update", post(genres::batch_update))
        .route("/api/genres/apply-analyses", post(genres::apply_genre_analyses))
        .route("/api/genres/cleanup-execute", post(genres::cleanup_execute))
        .route("/api/stream", get(media::stream))
        .route("/api/metadata", get(media::track_metadata))
}
