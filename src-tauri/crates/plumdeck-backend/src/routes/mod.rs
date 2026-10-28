//! HTTP routes, one module per Python router (setlists.py also carries
//! the recommendation routes; suggestions.py is the recommendation service's
//! genre suggestions).

pub mod dashboard;
pub mod genres;
pub mod lyrics;
pub mod media;
pub mod performance;
pub mod settings;
pub mod setlists;
pub mod suggestions;
pub mod tracks;
pub mod wordplay;

use axum::routing::{get, patch, post, put};
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
        .route("/api/setlists", get(setlists::get_setlists).post(setlists::post_setlist))
        .route("/api/setlists/{setlist_id}", put(setlists::put_setlist).delete(setlists::remove_setlist))
        .route("/api/setlists/{setlist_id}/tracks", get(setlists::get_setlist_tracks).post(setlists::post_setlist_tracks))
        .route("/api/setlists/{setlist_id}/export/m3u8", get(setlists::export_m3u8))
        .route("/api/setlists/{setlist_id}/export/validate", get(setlists::validate_export))
        .route("/api/recommendations/next", get(setlists::recommend_next))
        .route("/api/recommendations/next/page", get(setlists::recommend_next_page))
        .route("/api/recommendations/auto", post(setlists::auto_setlist))
        .route("/api/recommendations/path", post(setlists::path_setlist))
        .route("/api/setlist-tracks/{setlist_track_id}/wordplay", patch(setlists::patch_entry_wordplay))
        .route("/api/wordplay-pairs", get(wordplay::list))
        .route("/api/wordplay-pairs/{pair_id}/approve", post(wordplay::approve_route))
        .route("/api/wordplay-pairs/{pair_id}", patch(wordplay::patch_route).delete(wordplay::delete_route))
        .route("/api/tracks/{track_id}/performance-metadata", get(performance::get_route).put(performance::put_route))
        .route("/api/tracks/performance-metadata/cue-points", post(performance::cue_points_route))
        .route("/api/tracks/cues-rekordbox/import", post(performance::import_all_route))
        .route("/api/tracks/{track_id}/cues-rekordbox", post(performance::import_cues_route))
        .route("/api/tracks/{track_id}/grid-rekordbox", post(performance::grid_rekordbox_route))
        .route("/api/tracks/{track_id}/suggest-genre", get(suggestions::suggest_genre))
        .route("/api/genres/grouped-suggestions", get(suggestions::grouped_suggestions))
        .route("/api/genres/grouped-suggestions/{track_id}", get(suggestions::suggestions_for_track))
}
