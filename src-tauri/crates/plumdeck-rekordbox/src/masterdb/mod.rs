//! rekordbox's `master.db`, read-only.
//!
//! The database is SQLCipher 4. It is opened with `SQLITE_OPEN_READ_ONLY`, the
//! passphrase is applied, and `PRAGMA query_only=ON` is set before anything is
//! read, so no statement from this module can modify the library even by
//! mistake. Open one [`MasterDb`] per operation (like the Python backend) and
//! wrap multi-query reads in [`MasterDb::snapshot`] for a consistent view.
//!
//! Deleted rows (`rb_local_deleted <> 0`) are never returned.
//!
//! `agentRegistry` also holds account credentials: it is only ever read with
//! an explicit `registry_id = ?` (a unit test scans every statement here).

pub mod key;
pub mod model;
mod sql;
#[cfg(test)]
mod tests_db;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags, Row};

pub use key::Passphrase;
pub use model::*;

use crate::paths::{self, RegisteredPaths};

/// The schema version every query here was written against.
pub const KNOWN_DB_VERSION: &str = "6000";
/// `lookup_by_paths` caps the number of requested paths.
pub const MAX_LOOKUP_PATHS: usize = 256;
/// IN lists are chunked well below SQLite's parameter limit.
const SQL_CHUNK: usize = 250;
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum RekordboxError {
    /// No usable library: missing file, wrong key, not a rekordbox database.
    /// Callers fall back to plumdeck's own analysis.
    #[error("rekordbox library unavailable: {0}")]
    Unavailable(String),
    /// A query failed on an otherwise readable library.
    #[error("could not read the rekordbox library: {0}")]
    Query(#[from] rusqlite::Error),
    /// One audio path matches several rekordbox tracks.
    #[error("multiple rekordbox tracks match this audio path")]
    Ambiguous,
}

pub type Result<T> = std::result::Result<T, RekordboxError>;

/// A read-only, `query_only` connection to `master.db`.
pub struct MasterDb {
    conn: Connection,
    path: PathBuf,
}

impl std::fmt::Debug for MasterDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MasterDb").field("path", &self.path).finish_non_exhaustive()
    }
}

impl MasterDb {
    /// Opens `path` with `PLUMDECK_REKORDBOX_KEY` or the derived passphrase.
    pub fn open(path: &Path) -> Result<Self> {
        let key = Passphrase::resolve(&|name| std::env::var(name).ok())
            .ok_or_else(|| RekordboxError::Unavailable("no database key available".into()))?;
        Self::open_with_key(path, &key)
    }

    /// Opens the default location ([`crate::locate::default_master_db_path`]).
    pub fn open_default() -> Result<Self> {
        let path = crate::locate::default_master_db_path()
            .ok_or_else(|| RekordboxError::Unavailable("no rekordbox library location on this platform".into()))?;
        Self::open(&path)
    }

    /// Opens `path` with an explicit passphrase.
    pub fn open_with_key(path: &Path, key: &Passphrase) -> Result<Self> {
        if !path.is_file() {
            return Err(RekordboxError::Unavailable("master.db not found".into()));
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)
            .map_err(|_| RekordboxError::Unavailable("master.db could not be opened".into()))?;
        // rusqlite's pragma helpers use a form SQLCipher answers with "file is
        // not a database", so the statement is built here with the literal
        // escaped. Errors are replaced, so neither statement nor key can leak.
        let statement = format!("PRAGMA key = '{}'", key.expose().replace('\'', "''"));
        conn.execute_batch(&statement)
            .map_err(|_| RekordboxError::Unavailable("SQLCipher is not available".into()))?;
        drop(statement);
        conn.busy_timeout(BUSY_TIMEOUT)
            .and_then(|_| conn.pragma_update(None, "query_only", "ON"))
            .map_err(|_| RekordboxError::Unavailable("master.db could not be configured read-only".into()))?;
        let unlocked: rusqlite::Result<i64> =
            conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0));
        if unlocked.is_err() {
            return Err(RekordboxError::Unavailable(
                "master.db could not be unlocked (wrong key or not a rekordbox library)".into(),
            ));
        }
        let has_content: bool = conn
            .query_row(sql::HAS_CONTENT_TABLE, [], |row| row.get(0))
            .map_err(|_| RekordboxError::Unavailable("master.db is not a rekordbox library".into()))?;
        if !has_content {
            return Err(RekordboxError::Unavailable("master.db is not a rekordbox library".into()));
        }
        Ok(Self { conn, path: path.to_path_buf() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` inside one read transaction, so every query sees the same
    /// snapshot even while rekordbox writes to the WAL.
    pub fn snapshot<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN DEFERRED")?;
        let result = f(self);
        // A read transaction has nothing to keep; ROLLBACK cannot write.
        let end = self.conn.execute_batch("ROLLBACK");
        let value = result?;
        end?;
        Ok(value)
    }

    /// `djmdProperty.DBVersion` ("6000" for rekordbox 6 and 7).
    pub fn db_version(&self) -> Result<Option<String>> {
        Ok(self.conn.query_row(sql::DB_VERSION, [], |row| opt_text(row, 0)).or_else(none_if_no_rows)?)
    }

    /// Whether [`Self::db_version`] is the version these queries target.
    /// Callers should warn, not stop, on an unknown version.
    pub fn is_known_version(&self) -> Result<bool> {
        Ok(self.db_version()?.as_deref() == Some(KNOWN_DB_VERSION))
    }

    /// rekordbox's analysis share root (`SyncAnalysisDataRootPath`).
    pub fn analysis_data_root(&self) -> Result<Option<PathBuf>> {
        let value = self
            .conn
            .query_row(sql::REGISTRY_STR1, ["SyncAnalysisDataRootPath"], |row| opt_text(row, 0))
            .or_else(none_if_no_rows)?;
        Ok(value.filter(|v| !v.is_empty()).map(PathBuf::from))
    }

    // ---------------------------------------------------------------- content

    /// Every live `djmdContent` row.
    pub fn contents(&self) -> Result<Vec<Content>> {
        self.collect(&format!("{} WHERE c.rb_local_deleted = 0", sql::CONTENT_SELECT), [], content_row)
    }

    /// One live row by `ID`.
    pub fn content(&self, id: &str) -> Result<Option<Content>> {
        let rows = self.collect(
            &format!("{} WHERE c.ID = ? AND c.rb_local_deleted = 0", sql::CONTENT_SELECT),
            [id],
            content_row,
        )?;
        Ok(rows.into_iter().next())
    }

    /// Live rows whose `rb_local_usn` is above `usn` (incremental sync).
    pub fn contents_changed_since(&self, usn: i64) -> Result<Vec<Content>> {
        self.collect(
            &format!("{} WHERE c.rb_local_deleted = 0 AND c.rb_local_usn > ?", sql::CONTENT_SELECT),
            [usn],
            content_row,
        )
    }

    /// The largest `rb_local_usn` over all content rows, deleted ones included.
    pub fn max_content_usn(&self) -> Result<Option<i64>> {
        Ok(self.conn.query_row(sql::MAX_CONTENT_USN, [], |row| opt_int(row, 0))?)
    }

    /// `lookup_by_paths`: rows for exact file paths, keyed by the path as
    /// master.db spells it. Every Unicode spelling is queried, then identity
    /// is proven on the filesystem; unknown paths are simply absent.
    pub fn contents_by_paths<S: AsRef<str>>(&self, paths: &[S]) -> Result<BTreeMap<String, Content>> {
        let mut wanted: Vec<&str> = Vec::new();
        for path in paths.iter().map(AsRef::as_ref).filter(|p| !p.is_empty()) {
            if !wanted.contains(&path) {
                wanted.push(path);
            }
            if wanted.len() == MAX_LOOKUP_PATHS {
                break;
            }
        }
        let mut seen = HashSet::new();
        let spellings: Vec<String> =
            wanted.iter().flat_map(|p| paths::path_variants(p)).filter(|v| seen.insert(v.clone())).collect();
        let mut out = BTreeMap::new();
        for chunk in spellings.chunks(SQL_CHUNK) {
            for row in self.contents_where_path_in(chunk)? {
                let Some(filepath) = row.folder_path.clone() else { continue };
                if wanted.iter().any(|w| paths::same_path(&filepath, w)) {
                    out.insert(filepath, row);
                }
            }
        }
        Ok(out)
    }

    /// The live rows stored under exactly `filepath` (Windows: compared with
    /// forward slashes and case-insensitively, then proven on disk).
    pub fn contents_at_path(&self, filepath: &str) -> Result<Vec<Content>> {
        let rows = self.contents_where_path_in(&[filepath.to_owned()])?;
        if !paths::WINDOWS {
            return Ok(rows);
        }
        Ok(rows
            .into_iter()
            .filter(|row| row.folder_path.as_deref().is_some_and(|p| paths::same_path(filepath, p)))
            .collect())
    }

    /// The one live track at `filepath` (`_content_id`); several distinct IDs
    /// are an error rather than a guess.
    pub fn content_at_path(&self, filepath: &str) -> Result<Option<Content>> {
        let mut rows = self.contents_at_path(filepath)?;
        let ids: HashSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        if ids.len() > 1 {
            return Err(RekordboxError::Ambiguous);
        }
        Ok(rows.pop())
    }

    fn contents_where_path_in(&self, chunk: &[String]) -> Result<Vec<Content>> {
        if chunk.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; chunk.len()].join(",");
        let (column, params): (&str, Vec<String>) = if paths::WINDOWS {
            (sql::FOLDER_PATH_WINDOWS, chunk.iter().map(|p| p.replace('\\', "/")).collect())
        } else {
            ("c.FolderPath", chunk.to_vec())
        };
        let query = format!(
            "{} WHERE {column} IN ({placeholders}) AND c.rb_local_deleted = 0",
            sql::CONTENT_SELECT
        );
        self.collect(&query, rusqlite::params_from_iter(params.iter()), content_row)
    }

    /// `lookup_by_identities`: rows whose normalized title and artist both
    /// match one of `identities` (the fallback when no file path is known).
    pub fn contents_by_identities(&self, identities: &[(Option<&str>, Option<&str>)]) -> Result<Vec<Content>> {
        let wanted: HashSet<(String, String)> = identities
            .iter()
            .map(|(t, a)| (paths::normalize_text(*t), paths::normalize_text(*a)))
            .filter(|(t, _)| !t.is_empty())
            .collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.collect(
            &format!("{} WHERE c.rb_local_deleted = 0 AND c.FolderPath IS NOT NULL", sql::CONTENT_SELECT),
            [],
            content_row,
        )?;
        Ok(rows
            .into_iter()
            .filter(|row| {
                wanted.contains(&(
                    paths::normalize_text(row.title.as_deref()),
                    paths::normalize_text(row.artist.as_deref()),
                ))
            })
            .collect())
    }

    /// Every file path in the collection (`registered_paths`).
    pub fn registered_paths(&self) -> Result<RegisteredPaths> {
        let rows: Vec<Option<String>> = self.collect(sql::REGISTERED_PATHS, [], |row| opt_text(row, 0))?;
        Ok(RegisteredPaths::new(rows.into_iter().flatten()))
    }

    // ------------------------------------------------------------------- keys

    /// `djmdKey` (deleted keys included: content rows may still point at them).
    pub fn keys(&self) -> Result<Vec<KeyRow>> {
        self.collect(sql::KEYS, [], |row| {
            Ok(KeyRow { id: text(row, 0)?, scale_name: opt_text(row, 1)?, seq: opt_int(row, 2)? })
        })
    }

    /// `KeyID` → `ScaleName`.
    pub fn key_names(&self) -> Result<HashMap<String, String>> {
        Ok(self
            .keys()?
            .into_iter()
            .filter_map(|k| Some((k.id, k.scale_name?)))
            .collect())
    }

    // ------------------------------------------------------------------- cues

    /// Every live cue of one track, ordered by `Kind` then position.
    pub fn cues(&self, content_id: &str) -> Result<Vec<Cue>> {
        self.collect(&format!("{} AND ContentID = ? ORDER BY Kind, InMsec, ID", sql::CUE_SELECT), [content_id], cue_row)
    }

    /// Every live cue in the library.
    pub fn all_cues(&self) -> Result<Vec<Cue>> {
        self.collect(&format!("{} ORDER BY ContentID, Kind, InMsec, ID", sql::CUE_SELECT), [], cue_row)
    }

    // -------------------------------------------------------------- auto gain

    pub fn mixer_params(&self) -> Result<Vec<MixerParam>> {
        self.collect(sql::MIXER_SELECT, [], mixer_row)
    }

    pub fn mixer_param(&self, content_id: &str) -> Result<Option<MixerParam>> {
        let rows = self.collect(&format!("{} AND ContentID = ?", sql::MIXER_SELECT), [content_id], mixer_row)?;
        Ok(rows.into_iter().next())
    }

    // ------------------------------------------------------------ tags/colors

    pub fn my_tags(&self) -> Result<Vec<MyTag>> {
        self.collect(sql::MY_TAGS, [], |row| {
            Ok(MyTag {
                id: text(row, 0)?,
                seq: opt_int(row, 1)?,
                name: opt_text(row, 2)?,
                attribute: opt_int(row, 3)?,
                parent_id: opt_text(row, 4)?,
            })
        })
    }

    pub fn song_my_tags(&self) -> Result<Vec<SongMyTag>> {
        self.collect(sql::SONG_MY_TAGS, [], |row| {
            Ok(SongMyTag { my_tag_id: text(row, 0)?, content_id: text(row, 1)?, track_no: opt_int(row, 2)? })
        })
    }

    pub fn colors(&self) -> Result<Vec<ColorRow>> {
        self.collect(sql::COLORS, [], |row| {
            Ok(ColorRow {
                id: text(row, 0)?,
                color_code: opt_int(row, 1)?,
                sort_key: opt_int(row, 2)?,
                comment: opt_text(row, 3)?,
            })
        })
    }

    // -------------------------------------------------------------- playlists

    /// Live `djmdPlaylist` rows with their live track counts.
    pub fn playlists(&self) -> Result<Vec<Playlist>> {
        self.collect(sql::PLAYLISTS, [], |row| {
            Ok(Playlist {
                id: text(row, 0)?,
                name: opt_text(row, 1)?,
                parent_id: opt_text(row, 2)?,
                kind: PlaylistKind::from_attribute(opt_int(row, 3)?),
                seq: opt_int(row, 4)?,
                smart_list: opt_text(row, 5)?,
                track_count: opt_int(row, 6)?.unwrap_or(0),
            })
        })
    }

    /// `load_tree`: root-level nodes, children sorted by (seq, name, id), each
    /// carrying its `Folder/Sub/Name` path.
    pub fn playlist_tree(&self) -> Result<Vec<PlaylistNode>> {
        Ok(build_playlist_tree(self.playlists()?))
    }

    /// The tracks of one playlist in the DJ's order (`playlist_entries`).
    pub fn playlist_songs(&self, playlist_id: &str) -> Result<Vec<PlaylistSong>> {
        self.collect(sql::PLAYLIST_SONGS, [playlist_id], |row| {
            Ok(PlaylistSong {
                track_no: opt_int(row, 0)?.unwrap_or(0),
                content_id: text(row, 1)?,
                folder_path: opt_text(row, 2)?.unwrap_or_default(),
                title: opt_text(row, 3)?.unwrap_or_default(),
                artist: opt_text(row, 4)?.unwrap_or_default(),
            })
        })
    }

    // ---------------------------------------------------------------- history

    pub fn histories(&self) -> Result<Vec<History>> {
        self.collect(sql::HISTORIES, [], |row| {
            Ok(History {
                id: text(row, 0)?,
                name: opt_text(row, 1)?,
                parent_id: opt_text(row, 2)?,
                attribute: opt_int(row, 3)?,
                seq: opt_int(row, 4)?,
                date_created: opt_text(row, 5)?,
            })
        })
    }

    /// Every HISTORY playlist with its played files, oldest first
    /// (`_PlayHistoryCache`).
    pub fn play_sessions(&self) -> Result<Vec<PlaySession>> {
        let rows: Vec<(String, Option<String>, String)> =
            self.collect(sql::PLAY_SESSIONS, [], |row| Ok((text(row, 0)?, opt_text(row, 1)?, text(row, 2)?)))?;
        let mut sessions: Vec<PlaySession> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (history_id, created, path) in rows {
            let at = *index.entry(history_id.clone()).or_insert_with(|| {
                sessions.push(PlaySession { history_id, date_created: created, filepaths: Vec::new() });
                sessions.len() - 1
            });
            sessions[at].filepaths.push(path);
        }
        Ok(sessions)
    }

    // ------------------------------------------------------------------ misc

    /// Live `contentFile` rows (ANLZ files and artwork with absolute paths).
    pub fn content_files(&self) -> Result<Vec<ContentFile>> {
        self.collect(sql::CONTENT_FILES, [], content_file_row)
    }

    pub fn content_files_for(&self, content_id: &str) -> Result<Vec<ContentFile>> {
        self.collect(sql::CONTENT_FILES_FOR, [content_id], content_file_row)
    }

    pub fn recommend_likes(&self) -> Result<Vec<RecommendLike>> {
        self.collect(sql::RECOMMEND_LIKES, [], |row| {
            Ok(RecommendLike { content_id_1: text(row, 0)?, content_id_2: text(row, 1)?, like_rate: opt_int(row, 2)? })
        })
    }

    pub fn related_tracks(&self) -> Result<Vec<RelatedTracks>> {
        self.collect(sql::RELATED_TRACKS, [], |row| {
            Ok(RelatedTracks {
                id: text(row, 0)?,
                name: opt_text(row, 1)?,
                attribute: opt_int(row, 2)?,
                parent_id: opt_text(row, 3)?,
                criteria: opt_text(row, 4)?,
            })
        })
    }

    fn collect<T, P: rusqlite::Params>(
        &self,
        query: &str,
        params: P,
        map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>> {
        let mut statement = self.conn.prepare(query)?;
        let rows = statement.query_map(params, map)?;
        Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
    }

    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }
}

/// `clean_name`: drops control/format/unassigned characters and trims.
pub fn clean_name(name: Option<&str>) -> String {
    let kept: String = name.unwrap_or("").chars().filter(|&c| !is_unicode_other(c)).collect();
    plumdeck_core::compat::text::py_strip(&kept).to_owned()
}

/// Unicode category C* (Cc, Cf, Cs, Co, Cn) for the characters rekordbox
/// names can realistically contain. Cn (unassigned) is approximated by the
/// noncharacters; Cs cannot occur in a Rust `char`.
fn is_unicode_other(c: char) -> bool {
    let u = u32::from(c);
    c.is_control()
        || matches!(u, 0xAD | 0x600..=0x605 | 0x61C | 0x6DD | 0x70F | 0x890..=0x891 | 0x8E2 | 0x180E
            | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F | 0xFEFF
            | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
        || matches!(u, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
        || (u & 0xFFFE) == 0xFFFE
        || matches!(u, 0xFDD0..=0xFDEF)
}

fn build_playlist_tree(rows: Vec<Playlist>) -> Vec<PlaylistNode> {
    const ROOT: &str = "root";
    let mut nodes: Vec<PlaylistNode> = rows
        .into_iter()
        .map(|row| PlaylistNode {
            name: clean_name(row.name.as_deref()),
            parent_id: row.parent_id.filter(|p| !p.is_empty()).unwrap_or_else(|| ROOT.to_owned()),
            kind: row.kind,
            seq: row.seq.unwrap_or(0),
            track_count: row.track_count,
            path: String::new(),
            children: Vec::new(),
            id: row.id,
        })
        .collect();
    let ids: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    // Detach in an order that lets children be attached bottom-up: a node is
    // attached once all its own children are attached. Cycles (corrupt data)
    // fall back to the root so nothing is lost and nothing loops.
    let mut children_of: HashMap<String, Vec<PlaylistNode>> = HashMap::new();
    let mut roots = Vec::new();
    for node in nodes.drain(..) {
        if node.parent_id != node.id && ids.contains(&node.parent_id) {
            children_of.entry(node.parent_id.clone()).or_default().push(node);
        } else {
            roots.push(node);
        }
    }
    fn attach(node: &mut PlaylistNode, children_of: &mut HashMap<String, Vec<PlaylistNode>>, depth: usize) {
        if depth > 256 {
            return;
        }
        if let Some(mut children) = children_of.remove(&node.id) {
            for child in &mut children {
                attach(child, children_of, depth + 1);
            }
            node.children = children;
        }
    }
    for root in &mut roots {
        attach(root, &mut children_of, 0);
    }
    // Whatever is left belongs to a parent cycle: surface it at the top level.
    let mut orphans: Vec<PlaylistNode> = children_of.into_values().flatten().collect();
    orphans.sort_by(|a, b| a.id.cmp(&b.id));
    for mut orphan in orphans {
        orphan.children.clear();
        roots.push(orphan);
    }
    fn finish(children: &mut [PlaylistNode], prefix: &str) {
        children.sort_by(|a, b| (a.seq, &a.name, &a.id).cmp(&(b.seq, &b.name, &b.id)));
        for node in children {
            node.path = if prefix.is_empty() { node.name.clone() } else { format!("{prefix}/{}", node.name) };
            let path = node.path.clone();
            finish(&mut node.children, &path);
        }
    }
    finish(&mut roots, "");
    roots
}

// ------------------------------------------------------------------ row maps

fn content_row(row: &Row<'_>) -> rusqlite::Result<Content> {
    Ok(Content {
        id: text(row, 0)?,
        folder_path: opt_text(row, 1)?,
        title: opt_text(row, 2)?,
        artist: opt_text(row, 3)?,
        bpm_x100: opt_int(row, 4)?,
        key_id: opt_text(row, 5)?,
        key_name: opt_text(row, 6)?,
        length_s: opt_int(row, 7)?,
        analysis_data_path: opt_text(row, 8)?,
        rating: opt_int(row, 9)?,
        color_id: opt_text(row, 10)?,
        comment: opt_text(row, 11)?,
        file_type: opt_int(row, 12)?,
        rb_local_usn: opt_int(row, 13)?,
        analysis_updated: opt_text(row, 14)?,
        cue_updated: opt_text(row, 15)?,
    })
}

fn cue_row(row: &Row<'_>) -> rusqlite::Result<Cue> {
    let kind = opt_int(row, 2)?.unwrap_or(-1);
    let out_msec = opt_int(row, 5)?.filter(|&v| v >= 0);
    Ok(Cue {
        id: text(row, 0)?,
        content_id: text(row, 1)?,
        kind,
        slot: CueSlot::from_kind(kind),
        in_msec: opt_int(row, 3)?,
        in_frame: opt_int(row, 4)?,
        out_msec,
        color: opt_int(row, 6)?,
        color_table_index: opt_int(row, 7)?,
        active_loop: opt_int(row, 8)?.unwrap_or(0) != 0,
        comment: opt_text(row, 9)?,
        beat_loop: opt_int(row, 10)?.and_then(BeatLoop::from_packed),
    })
}

fn mixer_row(row: &Row<'_>) -> rusqlite::Result<MixerParam> {
    Ok(MixerParam {
        content_id: text(row, 0)?,
        gain_high: opt_int(row, 1)?.unwrap_or(0),
        gain_low: opt_int(row, 2)?.unwrap_or(0),
        peak_high: opt_int(row, 3)?.unwrap_or(0),
        peak_low: opt_int(row, 4)?.unwrap_or(0),
    })
}

fn content_file_row(row: &Row<'_>) -> rusqlite::Result<ContentFile> {
    Ok(ContentFile {
        content_id: text(row, 0)?,
        path: opt_text(row, 1)?,
        rb_local_path: opt_text(row, 2)?,
        size: opt_int(row, 3)?,
    })
}

/// A column read as text whatever its storage class (IDs are VARCHAR but
/// SQLite may hold them as integers).
fn opt_text(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<String>> {
    Ok(match row.get_ref(idx)? {
        ValueRef::Null => None,
        ValueRef::Integer(v) => Some(v.to_string()),
        ValueRef::Real(v) => Some(v.to_string()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
    })
}

fn text(row: &Row<'_>, idx: usize) -> rusqlite::Result<String> {
    Ok(opt_text(row, idx)?.unwrap_or_default())
}

/// A column read as an integer; numeric text is parsed, anything else is None.
fn opt_int(row: &Row<'_>, idx: usize) -> rusqlite::Result<Option<i64>> {
    Ok(match row.get_ref(idx)? {
        ValueRef::Integer(v) => Some(v),
        ValueRef::Real(v) if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.0e15 => Some(v as i64),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes).ok().and_then(|s| s.trim().parse().ok()),
        _ => None,
    })
}

fn none_if_no_rows<T>(error: rusqlite::Error) -> rusqlite::Result<Option<T>> {
    match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlist(id: &str, parent: &str, name: &str, seq: i64, kind: i64) -> Playlist {
        Playlist {
            id: id.into(),
            name: Some(name.into()),
            parent_id: Some(parent.into()),
            kind: PlaylistKind::from_attribute(Some(kind)),
            seq: Some(seq),
            smart_list: None,
            track_count: 0,
        }
    }

    #[test]
    fn tree_sorts_paths_and_survives_cycles() {
        let tree = build_playlist_tree(vec![
            playlist("2", "1", "B\u{7}", 2, 0),
            playlist("3", "1", "A", 2, 0),
            playlist("1", "root", " Folder ", 1, 1),
            playlist("4", "5", "x", 1, 0),
            playlist("5", "4", "y", 1, 0),
        ]);
        let folder = &tree[0];
        assert_eq!(folder.path, "Folder");
        let names: Vec<&str> = folder.children.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(names, ["Folder/A", "Folder/B"]);
        // The 4<->5 cycle is surfaced at the top level, not dropped.
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn clean_name_drops_controls() {
        assert_eq!(clean_name(Some("\u{200b} 現場物\u{1}/イベ2 ")), "現場物/イベ2");
        assert_eq!(clean_name(None), "");
    }
}
