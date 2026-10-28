//! `GET /api/dashboard` (backend/api/routers/system.py
//! `get_dashboard_stats`). `POST /api/system/save-file` and
//! `/api/system/reveal-file` are not ported: they move to Tauri at cutover
//! (S13, plan §2.2 items 14-15); `GET /api/` is dropped (§2.2 item 1).

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::PyValue;
use plumdeck_db::repos::dashboard::{dashboard_stats, DashboardStats, SetlistRow};

use crate::response::{datetime, dict, int, opt, text, PyJson};
use crate::AppState;

/// `jsonable_encoder(Setlist)`: every model field, datetimes in ISO form.
/// A NaN duration makes the whole answer a 500, as in Python.
fn setlist_json(row: &SetlistRow) -> PyValue {
    dict([
        ("id", int(row.id)),
        ("name", opt(row.name.clone(), text)),
        ("description", opt(row.description.clone(), text)),
        ("display_order", opt(row.display_order, int)),
        ("genre", opt(row.genre.clone(), text)),
        ("target_duration", opt(row.target_duration, PyValue::Float)),
        ("rating", opt(row.rating, int)),
        ("created_at", opt(row.created_at.as_ref(), datetime)),
        ("updated_at", opt(row.updated_at.as_ref(), datetime)),
    ])
}

fn stats_json(stats: &DashboardStats) -> PyValue {
    let genres = stats
        .genre_distribution
        .iter()
        .map(|genre| dict([("name", text(genre.name.clone())), ("count", int(genre.count))]))
        .collect();
    dict([
        ("total_tracks", int(stats.total_tracks)),
        ("analyzed_tracks", int(stats.analyzed_tracks)),
        ("unanalyzed_tracks", int(stats.unanalyzed_tracks())),
        ("embedding_pending_tracks", int(stats.embedding_pending_tracks)),
        ("genre_distribution", PyValue::List(genres)),
        ("unverified_genres_count", int(stats.unverified_genres_count)),
        ("lyrics_tracks_count", int(stats.lyrics_tracks_count)),
        ("recent_setlists", PyValue::List(stats.recent_setlists.iter().map(setlist_json).collect())),
        ("config", dict([("has_root_path", PyValue::Bool(stats.has_root_path))])),
    ])
}

pub async fn get_dashboard(State(state): State<AppState>) -> Response {
    let db = state.db.clone();
    state
        .blocking(move || db.lease().and_then(|conn| dashboard_stats(&conn)))
        .await
        .map(|stats| PyJson(stats_json(&stats)))
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::Method;
    use serde_json::json;

    use crate::router;
    use crate::test_support::{send, TestLibrary};

    #[tokio::test]
    async fn empty_and_populated_libraries() {
        let library = TestLibrary::new("dashboard");
        let app = || router(library.state());
        let answer = send(app(), Method::GET, "/api/dashboard", &[], "").await;
        assert_eq!(
            answer.json(),
            json!({
                "total_tracks": 0, "analyzed_tracks": 0, "unanalyzed_tracks": 0, "embedding_pending_tracks": 0,
                "genre_distribution": [],
                "unverified_genres_count": 0, "lyrics_tracks_count": 0, "recent_setlists": [],
                "config": {"has_root_path": false}
            })
        );
        library.execute(
            "INSERT INTO tracks (filepath, genre, bpm) VALUES ('/a', 'House', 120), ('/b', 'House', 0);
             INSERT INTO setlists (name, target_duration, rating, created_at, updated_at)
                VALUES ('Set', 1234.56, 4, '2026-01-02 03:04:05', '2026-01-02 03:04:05.000120');
             INSERT INTO settings VALUES ('root_path', '/music');",
        );
        let answer = send(app(), Method::GET, "/api/dashboard", &[], "").await;
        let body = answer.json();
        // Analyzed needs measured timbre and a stored waveform, not only a BPM.
        assert_eq!((body["analyzed_tracks"].clone(), body["unanalyzed_tracks"].clone()), (json!(0), json!(2)));
        assert_eq!(body["genre_distribution"], json!([{"name": "House", "count": 2}]));
        assert_eq!(body["unverified_genres_count"], json!(2));
        assert_eq!(
            body["recent_setlists"],
            json!([{
                "id": 1, "name": "Set", "description": null, "display_order": 0, "genre": null,
                "target_duration": f64::from(1234.56_f32), "rating": 4,
                "created_at": "2026-01-02T03:04:05", "updated_at": "2026-01-02T03:04:05.000120"
            }])
        );
        assert_eq!(body["config"], json!({"has_root_path": true}));
        // Python's json.dumps(allow_nan=False) fails on a NaN duration.
        library.execute("UPDATE setlists SET target_duration = 'nan'");
        let answer = send(app(), Method::GET, "/api/dashboard", &[], "").await;
        assert_eq!((answer.status.as_u16(), answer.body.as_str()), (500, "Internal Server Error"));
    }
}
