//! Every statement the master.db reader runs (fragments are completed with
//! `WHERE`/`AND`/`ORDER BY` clauses in `mod.rs`, never with data).

pub(super) const HAS_CONTENT_TABLE: &str =
    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'djmdContent')";

pub(super) const DB_VERSION: &str = "SELECT DBVersion FROM djmdProperty LIMIT 1";

/// agentRegistry also stores account credentials: only ever by registry_id.
pub(super) const REGISTRY_STR1: &str = "SELECT str_1 FROM agentRegistry WHERE registry_id = ?";

pub(super) const CONTENT_SELECT: &str = "SELECT c.ID, c.FolderPath, c.Title, a.Name, c.BPM, c.KeyID, \
     k.ScaleName, c.Length, c.AnalysisDataPath, c.Rating, c.ColorID, c.Commnt, c.FileType, \
     c.rb_local_usn, c.AnalysisUpdated, c.CueUpdated \
     FROM djmdContent c LEFT JOIN djmdArtist a ON a.ID = c.ArtistID \
     LEFT JOIN djmdKey k ON k.ID = c.KeyID";

pub(super) const FOLDER_PATH_WINDOWS: &str = "replace(c.FolderPath, char(92), '/') COLLATE NOCASE";

pub(super) const MAX_CONTENT_USN: &str = "SELECT max(rb_local_usn) FROM djmdContent";

pub(super) const REGISTERED_PATHS: &str =
    "SELECT FolderPath FROM djmdContent WHERE rb_local_deleted = 0 AND FolderPath IS NOT NULL";

pub(super) const KEYS: &str = "SELECT ID, ScaleName, Seq FROM djmdKey ORDER BY Seq, ID";

pub(super) const CUE_SELECT: &str = "SELECT ID, ContentID, Kind, InMsec, InFrame, OutMsec, Color, \
     ColorTableIndex, ActiveLoop, Comment, BeatLoopSize FROM djmdCue WHERE rb_local_deleted = 0";

pub(super) const MIXER_SELECT: &str = "SELECT ContentID, GainHigh, GainLow, PeakHigh, PeakLow \
     FROM djmdMixerParam WHERE rb_local_deleted = 0";

pub(super) const MY_TAGS: &str = "SELECT ID, Seq, Name, Attribute, ParentID FROM djmdMyTag \
     WHERE rb_local_deleted = 0 ORDER BY ParentID, Seq, ID";

pub(super) const SONG_MY_TAGS: &str = "SELECT MyTagID, ContentID, TrackNo FROM djmdSongMyTag \
     WHERE rb_local_deleted = 0";

pub(super) const COLORS: &str = "SELECT ID, ColorCode, SortKey, Commnt FROM djmdColor \
     WHERE rb_local_deleted = 0 ORDER BY SortKey, ID";

/// Track counts are grouped once: the correlated `count(*)` the Python
/// backend uses lets SQLite pick the `rb_local_deleted` index and takes ~30 s
/// on a 600-playlist library.
pub(super) const PLAYLISTS: &str = "SELECT p.ID, p.Name, p.ParentID, p.Attribute, p.Seq, p.SmartList, \
     coalesce(n.cnt, 0) FROM djmdPlaylist p \
     LEFT JOIN (SELECT PlaylistID, count(*) AS cnt FROM djmdSongPlaylist WHERE rb_local_deleted = 0 \
     GROUP BY PlaylistID) n ON n.PlaylistID = p.ID \
     WHERE p.rb_local_deleted = 0";

/// `+s.rb_local_deleted` keeps SQLite on the PlaylistID index.
pub(super) const PLAYLIST_SONGS: &str = "SELECT s.TrackNo, c.ID, c.FolderPath, c.Title, a.Name \
     FROM djmdSongPlaylist s JOIN djmdContent c ON c.ID = s.ContentID \
     LEFT JOIN djmdArtist a ON a.ID = c.ArtistID \
     WHERE s.PlaylistID = ? AND +s.rb_local_deleted = 0 AND c.rb_local_deleted = 0 \
     ORDER BY s.TrackNo, s.ID";

pub(super) const HISTORIES: &str = "SELECT ID, Name, ParentID, Attribute, Seq, DateCreated FROM djmdHistory \
     WHERE rb_local_deleted = 0 ORDER BY DateCreated, ID";

pub(super) const PLAY_SESSIONS: &str = "SELECT h.ID, h.DateCreated, c.FolderPath FROM djmdHistory h \
     JOIN djmdSongHistory s ON s.HistoryID = h.ID AND s.rb_local_deleted = 0 \
     JOIN djmdContent c ON c.ID = s.ContentID \
     WHERE h.rb_local_deleted = 0 AND c.FolderPath IS NOT NULL \
     ORDER BY h.DateCreated, h.ID, s.TrackNo";

pub(super) const CONTENT_FILES: &str = "SELECT ContentID, Path, rb_local_path, Size FROM contentFile \
     WHERE rb_local_deleted = 0";

/// `+rb_local_deleted` keeps SQLite on the ContentID index.
pub(super) const CONTENT_FILES_FOR: &str = "SELECT ContentID, Path, rb_local_path, Size FROM contentFile \
     WHERE ContentID = ? AND +rb_local_deleted = 0";

pub(super) const RECOMMEND_LIKES: &str = "SELECT ContentID1, ContentID2, LikeRate FROM djmdRecommendLike \
     WHERE rb_local_deleted = 0";

pub(super) const RELATED_TRACKS: &str = "SELECT ID, Name, Attribute, ParentID, Criteria FROM djmdRelatedTracks \
     WHERE rb_local_deleted = 0 ORDER BY Seq, ID";

#[cfg(test)]
pub(super) const ALL: &[&str] = &[
    HAS_CONTENT_TABLE,
    DB_VERSION,
    REGISTRY_STR1,
    CONTENT_SELECT,
    MAX_CONTENT_USN,
    REGISTERED_PATHS,
    KEYS,
    CUE_SELECT,
    MIXER_SELECT,
    MY_TAGS,
    SONG_MY_TAGS,
    COLORS,
    PLAYLISTS,
    PLAYLIST_SONGS,
    HISTORIES,
    PLAY_SESSIONS,
    CONTENT_FILES,
    CONTENT_FILES_FOR,
    RECOMMEND_LIKES,
    RELATED_TRACKS,
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Static guard: agentRegistry holds credentials, so every statement that
    /// touches it must select by `registry_id = ?`, and no statement in the
    /// crate's sources may mention it outside this module.
    #[test]
    fn agent_registry_is_only_read_by_registry_id() {
        for statement in ALL {
            let lower = statement.to_ascii_lowercase();
            if lower.contains("agentregistry") {
                assert!(lower.contains("where registry_id = ?"), "{statement}");
                assert!(!lower.contains('*'), "{statement}");
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        let mut scanned = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("masterdb/sql.rs") {
                    continue;
                }
                scanned += 1;
                let source = std::fs::read_to_string(&path).unwrap();
                for line in source.lines().filter(|l| l.to_ascii_lowercase().contains("agentregistry")) {
                    let trimmed = line.trim_start();
                    let comment = trimmed.starts_with("//");
                    let fixture = path.ends_with("masterdb/tests_db.rs");
                    assert!(comment || fixture, "agentRegistry outside sql.rs in {}: {line}", path.display());
                }
            }
        }
        assert!(scanned > 5);
    }

    #[test]
    fn no_statement_writes() {
        for statement in ALL {
            let upper = statement.to_ascii_uppercase();
            assert!(upper.trim_start().starts_with("SELECT"), "{statement}");
            let words: Vec<&str> = upper.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).collect();
            for verb in ["INSERT", "UPDATE", "DELETE", "REPLACE", "DROP", "ALTER", "CREATE", "ATTACH", "VACUUM", "PRAGMA"] {
                assert!(!words.contains(&verb), "{verb} in {statement}");
            }
        }
    }
}
