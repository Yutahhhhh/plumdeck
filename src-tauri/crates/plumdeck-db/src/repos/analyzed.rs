//! The "analyzed" rule and the embedding backlog as SQL
//! (backend/utils/ingestion.py `completed_analysis_condition`,
//! `valid_embedding_condition`, `pending_embedding_ids`), with the text
//! SQLAlchemy compiles them to.

/// `EMBEDDING_MODEL` (backend/domain/services/analysis/constants.py).
pub const EMBEDDING_MODEL: &str = "msd-musicnn-1:musicnn-16khz-v1";

/// `EMBEDDING_DIM`.
pub const EMBEDDING_DIM: usize = 200;

/// `completed_analysis_condition()`, correlated on `tracks`: duration > 0,
/// bpm > 0, measured timbre in [0, 1] and a stored waveform; light rows
/// also need the whole-track grid.
pub const COMPLETED_ANALYSIS_SQL: &str = "coalesce(tracks.bpm > 0 AND isfinite(tracks.bpm) AND tracks.duration > 0 \
AND isfinite(tracks.duration) AND tracks.energy >= 0 AND tracks.energy <= 1 AND isfinite(tracks.energy) \
AND tracks.danceability >= 0 AND tracks.danceability <= 1 AND isfinite(tracks.danceability) \
AND tracks.brightness >= 0 AND tracks.brightness <= 1 AND isfinite(tracks.brightness) \
AND tracks.noisiness >= 0 AND tracks.noisiness <= 1 AND isfinite(tracks.noisiness) \
AND (EXISTS (SELECT track_analyses.track_id FROM track_analyses WHERE track_analyses.track_id = tracks.id \
AND octet_length(track_analyses.waveform_u8) > 0 \
AND CASE WHEN (tracks.analysis_level = 'light') THEN coalesce((json_extract_string(CASE WHEN json_valid(track_analyses.features_extra_json) THEN track_analyses.features_extra_json END, '$.analysis_components.rhythm') NOT IN ('librosa-rhythm-light-v1', 'numpy-autocorrelation-light-v2')) AND octet_length(track_analyses.beats_f32) > 0, false) ELSE true END \
AND CASE WHEN (0 + CASE WHEN (tracks.energy = 0) THEN 1 ELSE 0 END + CASE WHEN (tracks.danceability = 0) THEN 1 ELSE 0 END + CASE WHEN (tracks.brightness = 0) THEN 1 ELSE 0 END + CASE WHEN (tracks.noisiness = 0) THEN 1 ELSE 0 END < 3) THEN true ELSE coalesce(json_extract_string(CASE WHEN json_valid(track_analyses.features_extra_json) THEN track_analyses.features_extra_json END, '$.analysis_components.timbre') IS NOT NULL, false) END)), false)";

/// `valid_embedding_condition()` for a query joining `track_embeddings`:
/// `EMBEDDING_DIM` finite numbers, not all zero.
pub const VALID_EMBEDDING_SQL: &str = "coalesce(len(try_cast(track_embeddings.embedding_json AS DOUBLE[])) = 200 \
AND list_bool_and(list_transform(try_cast(track_embeddings.embedding_json AS DOUBLE[]), x -> coalesce(isfinite(x), false))) \
AND list_max(list_transform(try_cast(track_embeddings.embedding_json AS DOUBLE[]), x -> abs(x))) > 0, false)";

/// `pending_embedding_ids()`: analyzed tracks without a valid vector of the
/// current model.
pub fn pending_embedding_ids_sql() -> String {
    format!(
        "SELECT tracks.id FROM tracks LEFT OUTER JOIN track_embeddings ON tracks.id = track_embeddings.track_id \
         WHERE {COMPLETED_ANALYSIS_SQL} AND (track_embeddings.track_id IS NULL OR track_embeddings.model_name IS NULL \
         OR track_embeddings.model_name != '{EMBEDDING_MODEL}' OR NOT {VALID_EMBEDDING_SQL})"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;

    #[test]
    fn analyzed_and_pending_embedding_rules() {
        let conn = Connection::open_in_memory().unwrap();
        crate::create_schema_v5(&conn).unwrap();
        let vector = |value: &str| format!("[{}]", vec![value; EMBEDDING_DIM].join(", "));
        conn.execute_batch(&format!(
            "INSERT INTO tracks (filepath, bpm, duration, energy, danceability, brightness, noisiness, analysis_level) VALUES
               ('/ok', 120, 60, 0.5, 0.5, 0.5, 0.5, NULL), ('/zeros', 120, 60, 0, 0, 0, 0.5, NULL),
               ('/measured', 120, 60, 0, 0, 0, 0.5, NULL), ('/light', 120, 60, 0.5, 0.5, 0.5, 0.5, 'light'),
               ('/no-waveform', 120, 60, 0.5, 0.5, 0.5, 0.5, NULL), ('/bad-bpm', 0, 60, 0.5, 0.5, 0.5, 0.5, NULL);
             INSERT INTO track_analyses (track_id, waveform_u8, beats_f32, features_extra_json) VALUES
               (1, '\\x01'::BLOB, NULL, '{{}}'), (2, '\\x01'::BLOB, NULL, '{{}}'),
               (3, '\\x01'::BLOB, NULL, '{{\"analysis_components\": {{\"timbre\": \"v\"}}}}'),
               (4, '\\x01'::BLOB, '\\x01'::BLOB, '{{\"analysis_components\": {{\"rhythm\": \"librosa-rhythm-light-v1\"}}}}'),
               (5, ''::BLOB, NULL, '{{}}'), (6, '\\x01'::BLOB, NULL, '{{}}');
             INSERT INTO track_embeddings (track_id, model_name, embedding_json) VALUES
               (1, '{EMBEDDING_MODEL}', '{}'), (3, '{EMBEDDING_MODEL}', '{}');",
            vector("0.5"),
            vector("0")
        ))
        .unwrap();
        let ids = |sql: &str| -> Vec<i64> {
            conn.prepare(sql).unwrap().query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
        };
        assert_eq!(ids(&format!("SELECT id FROM tracks WHERE {COMPLETED_ANALYSIS_SQL} ORDER BY id")), [1, 3]);
        // Track 3's vector is all zeros: still pending.
        assert_eq!(ids(&format!("{} ORDER BY tracks.id", pending_embedding_ids_sql())), [3]);
    }
}
