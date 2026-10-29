//! HTTP routes, one module per Python router (setlists.py also carries
//! the recommendation routes), plus the genre classifier and the optional
//! AI provider settings (plan §2.4), which have no Python counterpart.

pub mod ai;
pub mod assist;
pub mod csv_io;
pub mod dashboard;
pub mod file_tags;
pub mod genre_classifier;
pub mod genres;
pub mod ingest;
pub mod junction;
pub mod library;
pub mod lyrics;
pub mod mcp;
pub mod media;
pub mod metadata_jobs;
pub mod performance;
pub mod play;
pub mod rekordbox_view;
pub mod set_planner;
pub mod settings;
pub mod setlists;
pub mod tracks;
pub mod waveform_detail;
pub mod wordplay;
pub mod workflows;

use axum::routing::{delete, get, patch, post, put};
use axum::Router;

use crate::AppState;

/// Every ported `/api` route.
pub fn api() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(settings::get_settings).post(settings::update_setting))
        .route("/api/dashboard", get(dashboard::get_dashboard))
        .route("/api/library/attention", get(library::attention))
        .route("/api/library/attention/{kind}", post(library::fix_attention))
        .route("/api/rekordbox/unregistered", get(library::rekordbox_unregistered))
        .route("/api/rekordbox/unregistered/import", post(library::import_rekordbox_unregistered))
        .route("/api/system/save-file", post(dashboard::save_file))
        .route("/api/system/reveal-file", post(dashboard::reveal_file))
        .route("/api/tracks", get(tracks::get_tracks))
        .route("/api/tracks/page", get(tracks::get_tracks_page))
        .route("/api/tracks/count", get(tracks::get_tracks_count))
        .route("/api/tracks/ids", get(tracks::get_track_ids))
        .route("/api/tracks/{track_id}/similar", get(tracks::get_similar))
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
        .route("/api/play/playlists", get(play::local_playlists).post(play::create_local_playlist))
        .route("/api/play/playlists/{playlist_id}", patch(play::rename_local_playlist).delete(play::delete_local_playlist))
        .route("/api/play/playlists/{playlist_id}/tracks", get(play::local_playlist_tracks).post(play::add_local_playlist_track))
        .route("/api/play/playlists/{playlist_id}/tracks/{entry_id}", delete(play::remove_local_playlist_track))
        .route("/api/play/rekordbox/import", post(play::import_rekordbox_mirror))
        .route("/api/play/rekordbox/sources", get(play::mirror_sources))
        .route("/api/play/rekordbox/{source_id}/tree", get(play::mirror_tree))
        .route("/api/play/rekordbox/{source_id}/tree/page", get(play::mirror_tree_page))
        .route("/api/play/rekordbox/{source_id}/playlists/{playlist_id}/tracks", get(play::mirror_tracks))
        .route("/api/play/rekordbox/{source_id}/playlists/{playlist_id}/tracks/page", get(play::mirror_tracks_page))
        .route("/api/play/rekordbox/{source_id}/playlists/{playlist_id}/copy", post(play::copy_mirror_playlist))
        .route("/api/play/sessions", post(play::start_session))
        .route("/api/play/sessions/{session_id}/end", post(play::end_session))
        .route("/api/play/history", get(play::history).put(play::upsert_history))
        .route("/api/play/recordings", get(play::recordings).put(play::upsert_recording))
        .route("/api/play/recordings/formats", get(play::recording_formats))
        .route("/api/play/recordings/{recording_id}", patch(play::name_recording).delete(play::discard_recording))
        .route("/api/play/recordings/{recording_id}/audio", get(play::recording_audio))
        .route("/api/play/tracks/{track_id}/waveform-detail", get(waveform_detail::get_waveform_detail))
        .route("/api/workflows/summary", get(workflows::summary))
        .route("/api/workflows/media/diagnose", post(workflows::diagnose_media))
        .route("/api/workflows/media/repairs/{plan_id}/apply", post(workflows::apply_media_repair))
        .route("/api/workflows/media/repairs/{plan_id}/undo", post(workflows::undo_media_repair))
        .route("/api/workflows/version-groups", post(workflows::create_version_group))
        .route("/api/workflows/tracks/{track_id}/versions", get(workflows::versions_for_track))
        .route("/api/workflows/version-groups/{group_id}", patch(workflows::update_version_group))
        .route("/api/workflows/setlist-entries/{entry_id}/version", patch(workflows::swap_setlist_version))
        .route("/api/workflows/backups", post(workflows::backup))
        .route("/api/workflows/restore/inspect", post(workflows::restore_inspect))
        .route("/api/workflows/restore/apply", post(workflows::restore_apply))
        .route("/api/workflows/audio-presets", get(workflows::audio_presets).put(workflows::save_audio_preset))
        .route("/api/workflows/audio-presets/{preset_id}", delete(workflows::delete_audio_preset))
        .route("/api/workflows/controller-profiles", get(workflows::controller_profiles).put(workflows::save_controller_profile))
        .route("/api/workflows/usb/handoffs", get(workflows::list_usb_handoffs).post(workflows::create_usb_handoff))
        .route("/api/workflows/usb/devices", get(workflows::list_usb_devices))
        .route("/api/workflows/usb/devices/{device_id}/eject", post(workflows::eject_usb_device))
        .route("/api/workflows/usb/handoffs/{export_id}/duplicate", post(workflows::duplicate_usb_handoff))
        .route("/api/workflows/usb/handoffs/{export_id}/verify", post(workflows::verify_usb_handoff))
        .route("/api/workflows/recordings/{recording_id}/timeline", get(workflows::recording_timeline).put(workflows::replace_recording_timeline))
        .route("/api/workflows/recordings/{recording_id}/timeline/engine", post(workflows::upsert_recording_engine_timeline))
        .route("/api/workflows/recordings/{recording_id}/tracklist.txt", get(workflows::recording_tracklist))
        .route("/api/workflows/recordings/external", post(workflows::register_external_recording))
        .route("/api/ingest", post(ingest::ingest))
        .route("/api/ingest/status", get(ingest::ingest_status))
        .route("/api/ingest/cancel", post(ingest::cancel_ingest))
        .route("/ws/ingest", get(ingest::ws_ingest))
        .route("/ws/play-imports", get(ingest::ws_play_imports))
        .route("/api/grid-jobs/plan", post(ingest::plan_grid_job))
        .route("/api/grid-jobs", get(ingest::grid_job_status).post(ingest::start_grid_job))
        .route("/api/grid-jobs/{job_id}/{action}", post(ingest::control_grid_job))
        .route("/api/tracks/{track_id}/grid-analysis", post(ingest::grid_analysis))
        .route("/api/workflows/play/imports", get(ingest::list_play_imports).post(ingest::create_play_import))
        .route("/api/workflows/play/imports/{batch_id}", get(ingest::get_play_import))
        .route("/api/workflows/play/imports/{batch_id}/{action}", post(ingest::control_play_import))
        .route("/api/mcp/info", get(mcp::info))
        .route("/mcp", post(mcp::post).get(mcp::get_stream).delete(mcp::delete_session))
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
        .route("/api/tracks/{track_id}/waveform3", get(rekordbox_view::waveform3))
        .route("/api/tracks/{track_id}/rekordbox", get(rekordbox_view::analysis))
        .route("/api/tracks/{track_id}/memory-cues", get(rekordbox_view::get_memory_cues).put(rekordbox_view::put_memory_cues))
        .route("/api/tracks/{track_id}/memory-cues/rekordbox", post(rekordbox_view::import_memory_cues_route))
        .route("/api/tracks/{track_id}/suggest-genre", get(genre_classifier::suggest_genre))
        .route("/api/genres/classifier", get(genre_classifier::status))
        .route("/api/genres/classifier/train", post(genre_classifier::start_train))
        .route("/api/genres/classifier/predictions", get(genre_classifier::predictions))
        .route("/api/genres/classifier/accept", post(genre_classifier::accept))
        .route("/api/genres/classifier/similar/{track_id}", get(genre_classifier::similar))
        .route("/api/ai/settings", get(ai::get_settings).put(ai::put_settings))
        .route("/api/ai/keys/{provider}", put(ai::put_key).delete(ai::delete_key))
        .route("/api/ai/genre-suggestions", post(ai::genre_suggestions))
}
