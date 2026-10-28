//! HTTP routes, one module per Python router (setlists.py also carries
//! the recommendation routes; suggestions.py is the recommendation service's
//! genre suggestions).

pub mod assist;
pub mod csv_io;
pub mod dashboard;
pub mod file_tags;
pub mod genres;
pub mod lyrics;
pub mod media;
pub mod metadata_jobs;
pub mod performance;
pub mod settings;
pub mod setlists;
pub mod suggestions;
pub mod tracks;
pub mod wordplay;

use axum::routing::{delete, get, patch, post, put};
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
        .route("/api/genres/apply-to-files", post(file_tags::apply_to_files))
        .route("/api/metadata/update", post(metadata_jobs::start_update).patch(file_tags::update_content))
        .route("/api/metadata/fetch-artwork-info", post(file_tags::fetch_artwork_info))
        .route("/api/metadata/fetch-lyrics-single", post(metadata_jobs::fetch_lyrics_single))
        .route("/api/metadata/cancel", post(metadata_jobs::cancel_update))
        .route("/api/metadata/clear-cache", post(metadata_jobs::clear_cache))
        .route("/ws/metadata", get(metadata_jobs::ws_metadata))
        .route("/api/settings/export/csv", get(csv_io::export_csv))
        .route("/api/settings/import/analyze", post(csv_io::analyze_import))
        .route("/api/settings/import/execute", post(csv_io::execute_import))
        .route("/api/settings/metadata/export", get(csv_io::export_metadata))
        .route("/api/settings/metadata/import/analyze", post(csv_io::analyze_metadata))
        .route("/api/settings/metadata/import/execute", post(csv_io::execute_metadata))
        .route("/api/metadata", get(media::track_metadata))
        .route("/api/assist/library-status", get(assist::library_status))
        .route("/api/assist/decks/resolve", post(assist::resolve_decks))
        .route("/api/assist/recommendations", post(assist::recommendations))
        .route("/api/assist/routes", post(assist::routes))
        .route("/api/assist/artists", get(assist::artists))
        .route("/api/assist/search", post(assist::search))
        .route("/api/assist/window", put(assist::report_window))
        .route("/api/assist/agent", get(assist::agent_updates))
        .route("/api/assist/agent/list", delete(assist::dismiss_agent_list))
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
