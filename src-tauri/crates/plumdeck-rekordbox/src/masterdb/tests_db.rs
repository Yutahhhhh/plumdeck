//! The reader against a small synthetic SQLCipher database.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::*;

const TEST_KEY: &str = "synthetic-test-key";

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("plumdeck-rb-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SCHEMA: &str = "
CREATE TABLE djmdProperty (DBID VARCHAR(255), DBVersion VARCHAR(255));
CREATE TABLE agentRegistry (registry_id VARCHAR(255) PRIMARY KEY, str_1 VARCHAR(255), str_2 VARCHAR(255));
CREATE TABLE djmdArtist (ID VARCHAR(255) PRIMARY KEY, Name VARCHAR(255));
CREATE TABLE djmdKey (ID VARCHAR(255) PRIMARY KEY, ScaleName VARCHAR(255), Seq INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdContent (ID VARCHAR(255) PRIMARY KEY, FolderPath VARCHAR(255), Title VARCHAR(255),
  ArtistID VARCHAR(255), BPM INTEGER, KeyID VARCHAR(255), Length INTEGER, AnalysisDataPath VARCHAR(255),
  Rating INTEGER, ColorID VARCHAR(255), Commnt TEXT, FileType INTEGER, rb_local_usn BIGINT,
  AnalysisUpdated VARCHAR(255), CueUpdated VARCHAR(255), rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdCue (ID VARCHAR(255) PRIMARY KEY, ContentID VARCHAR(255), InMsec INTEGER, InFrame INTEGER,
  OutMsec INTEGER, Kind INTEGER, Color INTEGER, ColorTableIndex INTEGER, ActiveLoop INTEGER,
  Comment VARCHAR(255), BeatLoopSize INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdMixerParam (ID VARCHAR(255) PRIMARY KEY, ContentID VARCHAR(255), GainHigh INTEGER,
  GainLow INTEGER, PeakHigh INTEGER, PeakLow INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdMyTag (ID VARCHAR(255) PRIMARY KEY, Seq INTEGER, Name VARCHAR(255), Attribute INTEGER,
  ParentID VARCHAR(255), rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdSongMyTag (ID VARCHAR(255) PRIMARY KEY, MyTagID VARCHAR(255), ContentID VARCHAR(255),
  TrackNo INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdColor (ID VARCHAR(255) PRIMARY KEY, ColorCode INTEGER, SortKey INTEGER, Commnt VARCHAR(255),
  rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdPlaylist (ID VARCHAR(255) PRIMARY KEY, Seq INTEGER, Name VARCHAR(255), Attribute INTEGER,
  ParentID VARCHAR(255), SmartList TEXT, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdSongPlaylist (ID VARCHAR(255) PRIMARY KEY, PlaylistID VARCHAR(255), ContentID VARCHAR(255),
  TrackNo INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdHistory (ID VARCHAR(255) PRIMARY KEY, Seq INTEGER, Name VARCHAR(255), Attribute INTEGER,
  ParentID VARCHAR(255), DateCreated VARCHAR(255), rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdSongHistory (ID VARCHAR(255) PRIMARY KEY, HistoryID VARCHAR(255), ContentID VARCHAR(255),
  TrackNo INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE contentFile (ID VARCHAR(255) PRIMARY KEY, ContentID VARCHAR(255), Path VARCHAR(255),
  rb_local_path VARCHAR(255), Size INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdRecommendLike (ID VARCHAR(255) PRIMARY KEY, ContentID1 VARCHAR(255), ContentID2 VARCHAR(255),
  LikeRate INTEGER, rb_local_deleted TINYINT(1) DEFAULT 0);
CREATE TABLE djmdRelatedTracks (ID VARCHAR(255) PRIMARY KEY, Seq INTEGER, Name VARCHAR(255), Attribute INTEGER,
  ParentID VARCHAR(255), Criteria TEXT, rb_local_deleted TINYINT(1) DEFAULT 0);

INSERT INTO djmdProperty VALUES ('db', '6000');
INSERT INTO agentRegistry VALUES ('SyncAnalysisDataRootPath', '/lib/share', NULL);
INSERT INTO agentRegistry VALUES ('secret_token', 'must-not-be-read', 'x');
INSERT INTO djmdArtist VALUES ('a1', 'Artist One');
INSERT INTO djmdKey VALUES ('k1', 'Fm', 1, 0), ('k2', '8A', 2, 0);
INSERT INTO djmdContent VALUES
  ('100', '/music/Caf\u{e9}.mp3', 'Song', 'a1', 12800, 'k1', 300, '/PIONEER/USBANLZ/aa/bb/ANLZ0000.DAT',
   3, '0', 'note', 1, 50, '1', '2', 0),
  ('101', '/music/b.mp3', 'Other', NULL, 0, NULL, 200, NULL, NULL, NULL, NULL, 1, 60, NULL, NULL, 0),
  ('102', '/music/deleted.mp3', 'Gone', NULL, 12000, 'k2', 100, NULL, NULL, NULL, NULL, 1, 70, NULL, NULL, 1),
  (103, '/music/dup.mp3', 'Dup A', NULL, 12000, 'k2', 100, NULL, NULL, NULL, NULL, 1, 80, NULL, NULL, 0),
  ('104', '/music/dup.mp3', 'Dup B', NULL, 12000, 'k2', 100, NULL, NULL, NULL, NULL, 1, 90, NULL, NULL, 0);
INSERT INTO djmdCue VALUES
  ('c1', '100', 1000, 150, -1, 1, -1, 0, 0, 'Drop', NULL, 0),
  ('c2', '100', 2000, 300, 4000, 0, 255, 26, 1, '', 262145, 0),
  ('c3', '100', 3000, 450, -1, 10, -1, NULL, NULL, NULL, NULL, 0),
  ('c4', '100', 5000, 750, -1, 9, -1, 0, 0, 'old', NULL, 1);
INSERT INTO djmdMixerParam VALUES ('m1', '100', 16128, 0, 16256, 0, 0);
INSERT INTO djmdMyTag VALUES ('t0', 1, 'Genre', 1, 'root', 0), ('t1', 1, 'Techno', 0, 't0', 0);
INSERT INTO djmdSongMyTag VALUES ('s1', 't1', '100', 1, 0), ('s2', 't1', '101', 2, 1);
INSERT INTO djmdColor VALUES ('1', 1, 1, 'Pink', 0);
INSERT INTO djmdPlaylist VALUES
  ('p1', 1, 'Folder', 1, 'root', NULL, 0),
  ('p2', 2, 'Set B', 0, 'p1', NULL, 0),
  ('p3', 1, 'Set A', 0, 'p1', NULL, 0),
  ('p4', 3, 'Smart', 4, 'root', '<NODE/>', 0),
  ('p5', 4, 'Deleted', 0, 'root', NULL, 1);
INSERT INTO djmdSongPlaylist VALUES ('sp1', 'p3', '101', 2, 0), ('sp2', 'p3', '100', 1, 0), ('sp3', 'p3', '102', 3, 0),
  ('sp4', 'p3', '100', 4, 1);
INSERT INTO djmdHistory VALUES ('h2', 2, '2024-02-01', 0, 'root', '2024-02-01', 0),
  ('h1', 1, '2024-01-01', 0, 'root', '2024-01-01', 0);
INSERT INTO djmdSongHistory VALUES ('sh1', 'h2', '101', 1, 0), ('sh2', 'h1', '100', 2, 0), ('sh3', 'h1', '101', 1, 0),
  ('sh4', 'h1', '100', 3, 1);
INSERT INTO contentFile VALUES ('f1', '100', '/PIONEER/USBANLZ/aa/bb/ANLZ0000.DAT', '/lib/share/PIONEER/USBANLZ/aa/bb/ANLZ0000.DAT', 10, 0);
INSERT INTO djmdRecommendLike VALUES ('r1', '100', '101', 5, 0);
INSERT INTO djmdRelatedTracks VALUES ('rt1', 1, 'BPM + Key', 0, 'root', '{\"bpm\":1}', 0);
";

fn build(dir: &Path) -> PathBuf {
    let path = dir.join("master.db");
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "key", TEST_KEY).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    drop(conn);
    path
}

fn key() -> Passphrase {
    Passphrase::new(TEST_KEY).unwrap()
}

#[test]
fn reads_every_table() {
    let dir = TempDir::new("read");
    let path = build(&dir.0);
    let before = std::fs::read(&path).unwrap();
    let db = MasterDb::open_with_key(&path, &key()).unwrap();

    assert_eq!(db.db_version().unwrap().as_deref(), Some("6000"));
    assert!(db.is_known_version().unwrap());
    assert_eq!(db.analysis_data_root().unwrap(), Some(PathBuf::from("/lib/share")));

    let contents = db.contents().unwrap();
    assert_eq!(contents.len(), 4, "deleted row excluded");
    let song = db.content("100").unwrap().unwrap();
    assert_eq!(song.bpm(), Some(128.0));
    assert_eq!(song.key_name.as_deref(), Some("Fm"));
    assert_eq!(song.artist.as_deref(), Some("Artist One"));
    assert_eq!(song.length_s, Some(300));
    assert_eq!(song.rating, Some(3));
    assert_eq!(song.comment.as_deref(), Some("note"));
    assert_eq!(db.content("101").unwrap().unwrap().bpm(), None, "BPM 0 is not analysed");
    assert_eq!(db.content("102").unwrap(), None);
    // An integer-typed ID still reads as text.
    assert_eq!(db.content("103").unwrap().unwrap().id, "103");

    assert_eq!(db.max_content_usn().unwrap(), Some(90));
    let changed: Vec<String> = db.contents_changed_since(60).unwrap().into_iter().map(|c| c.id).collect();
    assert_eq!(changed.len(), 2);

    assert_eq!(db.key_names().unwrap().get("k2").map(String::as_str), Some("8A"));

    let cues = db.cues("100").unwrap();
    assert_eq!(cues.len(), 3, "deleted cue excluded");
    let memory = cues.iter().find(|c| c.slot == CueSlot::Memory).unwrap();
    assert!(memory.is_loop());
    assert_eq!(memory.out_msec, Some(4000));
    assert_eq!(memory.beat_loop.and_then(|l| l.beats()), Some(4.0));
    assert_eq!(memory.color_table_index, Some(26));
    assert!(memory.active_loop);
    let a = cues.iter().find(|c| c.slot == CueSlot::Hot(0)).unwrap();
    assert_eq!((a.in_msec, a.out_msec, a.comment.as_deref()), (Some(1000), None, Some("Drop")));
    assert_eq!(cues.iter().find(|c| c.kind == 10).unwrap().slot.letter(), Some('I'));
    assert_eq!(db.all_cues().unwrap().len(), 3);

    let gain = db.mixer_param("100").unwrap().unwrap();
    assert_eq!(gain.gain(), Some(f32::from_bits(16128 << 16)));
    assert_eq!(db.mixer_params().unwrap().len(), 1);

    assert_eq!(db.my_tags().unwrap().len(), 2);
    assert_eq!(db.song_my_tags().unwrap().len(), 1);
    assert_eq!(db.colors().unwrap()[0].comment.as_deref(), Some("Pink"));

    let tree = db.playlist_tree().unwrap();
    assert_eq!(tree.iter().map(|n| n.path.as_str()).collect::<Vec<_>>(), ["Folder", "Smart"]);
    let children: Vec<&str> = tree[0].children.iter().map(|n| n.path.as_str()).collect();
    assert_eq!(children, ["Folder/Set A", "Folder/Set B"]);
    assert_eq!(tree[0].children[0].track_count, 3);
    assert_eq!(tree[1].kind, PlaylistKind::Smart);
    let songs = db.playlist_songs("p3").unwrap();
    let order: Vec<&str> = songs.iter().map(|s| s.content_id.as_str()).collect();
    assert_eq!(order, ["100", "101"], "ordered by TrackNo, deleted track and slot excluded");

    let sessions = db.play_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].history_id, "h1");
    assert_eq!(sessions[0].filepaths, ["/music/b.mp3", "/music/Caf\u{e9}.mp3"]);
    assert_eq!(db.histories().unwrap().len(), 2);

    assert_eq!(db.content_files_for("100").unwrap().len(), 1);
    assert_eq!(db.content_files().unwrap().len(), 1);
    assert_eq!(db.recommend_likes().unwrap()[0].like_rate, Some(5));
    assert_eq!(db.related_tracks().unwrap()[0].name.as_deref(), Some("BPM + Key"));

    assert!(db.registered_paths().unwrap().contains_exact("/music/b.mp3"));
    assert!(!db.registered_paths().unwrap().contains_exact("/music/deleted.mp3"));

    let found = db.contents_by_identities(&[(Some(" SONG "), Some("artist one"))]).unwrap();
    assert_eq!(found.len(), 1);

    // Snapshots nest the reads in one transaction and leave nothing open.
    let n = db.snapshot(|db| Ok(db.contents()?.len() + db.all_cues()?.len())).unwrap();
    assert_eq!(n, 7);
    assert!(db.connection().is_autocommit());

    drop(db);
    assert_eq!(std::fs::read(&path).unwrap(), before, "reading never changes the file");
}

#[test]
fn path_lookups_match_python() {
    let dir = TempDir::new("paths");
    let path = build(&dir.0);
    let db = MasterDb::open_with_key(&path, &key()).unwrap();
    assert_eq!(db.content_at_path("/music/b.mp3").unwrap().unwrap().id, "101");
    assert_eq!(db.content_at_path("/music/missing.mp3").unwrap(), None);
    assert_eq!(db.content_at_path("/music/deleted.mp3").unwrap(), None);
    assert!(matches!(db.content_at_path("/music/dup.mp3"), Err(RekordboxError::Ambiguous)));
    let found = db.contents_by_paths(&["/music/b.mp3", "/music/b.mp3", "", "/music/zzz.mp3"]).unwrap();
    assert_eq!(found.keys().collect::<Vec<_>>(), ["/music/b.mp3"]);
}

#[test]
fn writes_are_refused() {
    let dir = TempDir::new("ro");
    let path = build(&dir.0);
    let db = MasterDb::open_with_key(&path, &key()).unwrap();
    let conn = db.connection();
    assert!(conn.execute("UPDATE djmdContent SET Title = 'x'", []).is_err());
    assert!(conn.execute("INSERT INTO djmdColor (ID) VALUES ('9')", []).is_err());
    assert!(conn.execute_batch("PRAGMA query_only=OFF; DELETE FROM djmdCue").is_err());
}

#[test]
fn unusable_libraries_are_unavailable() {
    let dir = TempDir::new("bad");
    let missing = dir.0.join("nope.db");
    assert!(matches!(MasterDb::open_with_key(&missing, &key()), Err(RekordboxError::Unavailable(_))));

    let path = build(&dir.0);
    let wrong = Passphrase::new("wrong-key").unwrap();
    let error = MasterDb::open_with_key(&path, &wrong).unwrap_err();
    assert!(matches!(error, RekordboxError::Unavailable(_)));
    let shown = format!("{error} {error:?}");
    assert!(!shown.contains("wrong-key") && !shown.contains(TEST_KEY), "key leaked: {shown}");

    let plain = dir.0.join("plain.db");
    Connection::open(&plain).unwrap().execute_batch("CREATE TABLE t (x)").unwrap();
    assert!(matches!(MasterDb::open_with_key(&plain, &key()), Err(RekordboxError::Unavailable(_))));
    let not_db = dir.0.join("garbage.db");
    std::fs::write(&not_db, b"not a database at all").unwrap();
    assert!(matches!(MasterDb::open_with_key(&not_db, &key()), Err(RekordboxError::Unavailable(_))));
}

