"""
infra/database/compaction.ensure_healthy_db のテスト。

- 旧スキーマ (v3, JSON カラム) の DB を v4 (BLOB カラム) へファイル再構築で移行できる
- データ / シーケンス現在値 / 波形のダウンサンプルが保たれる
- 既に v4 のファイルは肥大していなければ何もしない
- 肥大した v4 ファイルは変換なしで再構築 (コンパクション) される
- features_extra_json は許可リストで作り直される (旧形式の行があれば 1 回だけ再構築)
"""
import glob
import json
import os

import duckdb
import pytest

import infra.database.compaction as compaction
from domain.models.track import TrackAnalysis
from infra.database.compaction import ensure_healthy_db
from infra.database.schema import get_schema_statements
from utils.features_extra import PERSISTED_KEYS


def _make_legacy_v3_db(path: str, n_tracks: int = 5) -> None:
    con = duckdb.connect(path)
    for seq in ("seq_tracks_id", "seq_setlists_id", "seq_setlist_tracks_id"):
        con.execute(f"CREATE SEQUENCE {seq} START 1")
    con.execute(
        "CREATE TABLE tracks (id INTEGER PRIMARY KEY DEFAULT nextval('seq_tracks_id'), "
        "filepath VARCHAR UNIQUE NOT NULL, title VARCHAR, artist VARCHAR, genre VARCHAR, bpm FLOAT)"
    )
    con.execute(
        "CREATE TABLE track_analyses (track_id INTEGER PRIMARY KEY, beat_positions JSON, "
        "waveform_peaks JSON, features_extra_json VARCHAR DEFAULT '{}')"
    )
    con.execute(
        "CREATE TABLE track_embeddings (track_id INTEGER PRIMARY KEY, model_name VARCHAR DEFAULT "
        "'musicnn', embedding_json VARCHAR DEFAULT '[]', updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP)"
    )
    con.execute("CREATE TABLE lyrics (track_id INTEGER PRIMARY KEY, content VARCHAR DEFAULT '')")
    con.execute(
        "CREATE TABLE setlists (id INTEGER PRIMARY KEY DEFAULT nextval('seq_setlists_id'), name VARCHAR NOT NULL)"
    )
    con.execute(
        "CREATE TABLE setlist_tracks (id INTEGER PRIMARY KEY DEFAULT nextval('seq_setlist_tracks_id'), "
        "setlist_id INTEGER, track_id INTEGER, position INTEGER)"
    )
    con.execute("CREATE TABLE settings (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL)")
    con.execute("CREATE TABLE schema_info (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL)")
    con.execute("INSERT INTO schema_info VALUES ('version', '3')")

    waveform = [(j % 100) / 100.0 for j in range(2000)]
    beats = [round(x * 0.5, 3) for x in range(300)]
    for i in range(1, n_tracks + 1):
        con.execute(
            "INSERT INTO tracks (filepath, title, artist, genre, bpm) VALUES (?, ?, ?, ?, ?)",
            [f"/m/{i}.mp3", f"T{i}", "A", "House", 120.0 + i],
        )
        con.execute(
            "INSERT INTO track_analyses VALUES (?, ?, ?, ?)",
            [i, json.dumps(beats), json.dumps(waveform),
             json.dumps({"bpm_confidence": 0.9, "analysis_components": {"embedding": "musicnn-16khz-v1"}})],
        )
        con.execute(
            "INSERT INTO track_embeddings (track_id, embedding_json) VALUES (?, ?)",
            [i, json.dumps([0.1] * 200)],
        )
    con.execute("INSERT INTO setlists (name) VALUES ('S1')")
    con.execute("INSERT INTO setlist_tracks (setlist_id, track_id, position) VALUES (1, 2, 0)")
    con.close()


def test_migrates_v3_to_v4_and_preserves_data(tmp_path):
    db = str(tmp_path / "legacy.duckdb")
    _make_legacy_v3_db(db, n_tracks=5)

    ensure_healthy_db(db)

    con = duckdb.connect(db)
    try:
        assert con.execute("SELECT value FROM schema_info WHERE key='version'").fetchone()[0] == "5"
        cols = {
            r[0]
            for r in con.execute(
                "SELECT column_name FROM information_schema.columns WHERE table_name='track_analyses'"
            ).fetchall()
        }
        assert cols == {"track_id", "beats_f32", "waveform_u8", "features_extra_json"}
        assert con.execute("SELECT count(*) FROM tracks").fetchone()[0] == 5
        assert con.execute("SELECT count(*) FROM track_embeddings").fetchone()[0] == 5
        assert con.execute("SELECT setlist_id, track_id, position FROM setlist_tracks").fetchone() == (1, 2, 0)

        wu8, bf32, fx = con.execute(
            "SELECT waveform_u8, beats_f32, features_extra_json FROM track_analyses WHERE track_id=1"
        ).fetchone()
        ta = TrackAnalysis(track_id=1)
        ta.waveform_u8, ta.beats_f32 = wu8, bf32
        assert len(ta.waveform_peaks) == 500          # 2000 点 -> 500 点にダウンサンプル
        assert len(ta.beat_positions) == 300
        assert ta.beat_positions[-1] == pytest.approx(149.5)
        # 読み手のないキーは移行で落ち、許可リストのキーは残る
        assert json.loads(fx) == {"analysis_components": {"embedding": "musicnn-16khz-v1"}}

        # シーケンスは既存 id の続きから採番される
        con.execute("INSERT INTO tracks (filepath, title, artist, genre, bpm) VALUES ('/m/new.mp3','N','A','House',130)")
        assert con.execute("SELECT id FROM tracks WHERE filepath='/m/new.mp3'").fetchone()[0] == 6
    finally:
        con.close()

    # 旧ファイルは .bak として残る
    assert len(glob.glob(db + ".bak-v5-*")) == 1


def test_second_run_is_noop(tmp_path):
    db = str(tmp_path / "legacy.duckdb")
    _make_legacy_v3_db(db, n_tracks=3)

    ensure_healthy_db(db)
    size_after_migrate = os.path.getsize(db)
    bak_count = len(glob.glob(db + ".bak-v5-*"))

    ensure_healthy_db(db)  # 2 回目

    assert os.path.getsize(db) == size_after_migrate
    assert not os.path.exists(db + ".rebuild")
    assert len(glob.glob(db + ".bak-v5-*")) == bak_count


def test_missing_file_is_noop(tmp_path):
    ensure_healthy_db(str(tmp_path / "does_not_exist.duckdb"))  # 例外を投げない


def test_compacts_bloated_v4_file(tmp_path, monkeypatch):
    db = str(tmp_path / "v4.duckdb")
    con = duckdb.connect(db)
    for stmt in get_schema_statements():
        con.execute(stmt)
    con.execute(
        "INSERT INTO schema_info VALUES ('version','4') ON CONFLICT (key) DO UPDATE SET value=excluded.value"
    )
    for i in range(1, 4):
        con.execute(
            "INSERT INTO tracks (filepath, title, artist, genre, bpm) VALUES (?, ?, ?, ?, ?)",
            [f"/m/{i}.mp3", f"T{i}", "A", "House", 120.0],
        )
        con.execute(
            "INSERT INTO track_analyses (track_id, beats_f32, waveform_u8, features_extra_json) VALUES (?, ?, ?, ?)",
            [i, b"\x00" * 40, bytes(range(256)), "{}"],
        )
    con.close()

    # しきい値を下げて肥大とみなさせる
    monkeypatch.setattr(compaction, "COMPACT_MIN_BYTES", 1)
    monkeypatch.setattr(compaction, "COMPACT_BYTES_PER_TRACK", 1)
    monkeypatch.setattr(compaction, "COMPACT_RATIO_LIMIT", 1)

    ensure_healthy_db(db)

    con = duckdb.connect(db)
    try:
        assert con.execute("SELECT value FROM schema_info WHERE key='version'").fetchone()[0] == "5"
        assert con.execute("SELECT count(*) FROM tracks").fetchone()[0] == 3
        assert con.execute(
            "SELECT octet_length(waveform_u8) FROM track_analyses WHERE track_id=1"
        ).fetchone()[0] == 256
    finally:
        con.close()
    assert len(glob.glob(db + ".bak-v5-*")) == 1


# --------------------------------------------------------------------------- #
# features_extra の許可リスト (A1 で保存をやめたキーを既存行からも除く)
# --------------------------------------------------------------------------- #
_BEATS = [round(0.5 + x * 0.5, 3) for x in range(64)]
_GRID = {"bpm": 120.0, "first_beat_ms": 500.0, "beat_times_ms": None, "source": "analysis", "confidence": 3.08}
_AUTHORITY = {"source": "rekordbox", "rekordbox_id": "8242653",
              "synced_at": "2026-09-07T05:43:13.998552+00:00", "match_method": "path"}
# 実 DB の旧形式の行と同じ形: 拍・波形の重複配列、旧キー、記録用キー、許可リストのキー
_LEGACY_FX = {
    "loudness_range_db": 10.16, "spectral_flux_raw": 0.18, "spectral_rolloff_hz": 3747.0,
    "bpm_confidence": 1.95, "key_strength": 0.87, "bpm_raw": 122.51,
    "beat_positions": _BEATS, "waveform_peaks": [(j % 100) / 100.0 for j in range(2000)],
    "embedding_sample_rate": 16000, "embedding_pipeline_version": "musicnn-16khz-v1",
    "analysis_source": {"size": 6220612, "mtime_ns": 1766809083878446587},
    "analysis_level": "full", "analysis_profile": "full",
    "analysis_components": {"rhythm": "rhythm-attacks-v4", "key": "key-v1", "timbre": "timbre-v1"},
    "playback_grid": _GRID, "playback_grid_version": 2, "bpm_authority": _AUTHORITY,
}
_KEPT_FX = {k: _LEGACY_FX[k] for k in
            ("analysis_components", "playback_grid", "playback_grid_version", "bpm_authority")}
# 既に許可リストだけの行 (区切りの空白なし) は 1 バイトも変えない
_CLEAN_FX_TEXT = '{"analysis_components":{"embedding":"musicnn-16khz-v1"},"analysis_bpm":121.5}'


def _make_v5_db(path: str, fx_rows: list) -> None:
    con = duckdb.connect(path)
    for stmt in get_schema_statements():
        con.execute(stmt)
    con.execute(
        "INSERT INTO schema_info VALUES ('version','5') ON CONFLICT (key) DO UPDATE SET value=excluded.value"
    )
    for i, fx in enumerate(fx_rows, start=1):
        con.execute(
            "INSERT INTO tracks (filepath, title, artist, genre, bpm, key) VALUES (?, ?, ?, ?, ?, ?)",
            [f"/m/{i}.mp3", f"T{i}", "A", "House", 120.0 + i, "F minor"],
        )
        con.execute(
            "INSERT INTO track_analyses (track_id, beats_f32, waveform_u8, features_extra_json) VALUES (?, ?, ?, ?)",
            [i, bytes([i]) * 4 * 64, bytes([i]) * 500, fx],
        )
        con.execute(
            "INSERT INTO track_embeddings (track_id, embedding_json) VALUES (?, ?)", [i, json.dumps([0.1 * i] * 200)]
        )
    con.close()


def _snapshot(path: str) -> dict:
    con = duckdb.connect(path, read_only=True)
    try:
        return {
            "tracks": con.execute("SELECT * FROM tracks ORDER BY id").fetchall(),
            "embeddings": con.execute(
                "SELECT track_id, model_name, embedding_json FROM track_embeddings ORDER BY track_id"
            ).fetchall(),
            "blobs": con.execute(
                "SELECT track_id, beats_f32, waveform_u8 FROM track_analyses ORDER BY track_id"
            ).fetchall(),
            "fx": dict(con.execute("SELECT track_id, features_extra_json FROM track_analyses").fetchall()),
            "marker": con.execute(
                "SELECT value FROM schema_info WHERE key = ?", [compaction.FX_CLEAN_KEY]
            ).fetchone(),
        }
    finally:
        con.close()


def test_rebuild_drops_features_extra_keys_outside_allow_list(tmp_path):
    db = str(tmp_path / "legacy-fx.duckdb")
    _make_v5_db(db, [json.dumps(_LEGACY_FX), _CLEAN_FX_TEXT, "{}"])
    before = _snapshot(db)

    ensure_healthy_db(db)  # 肥大していなくても旧形式の行があれば再構築する

    after = _snapshot(db)
    assert len(glob.glob(db + ".bak-v5-*")) == 1
    # 行数・BLOB・他テーブルの値は変わらない
    assert after["tracks"] == before["tracks"]
    assert after["embeddings"] == before["embeddings"]
    assert after["blobs"] == before["blobs"]
    # 許可リストのキーだけが同じ値で残る
    assert json.loads(after["fx"][1]) == _KEPT_FX
    assert set(json.loads(after["fx"][1])) <= set(PERSISTED_KEYS)
    assert after["fx"][2] == _CLEAN_FX_TEXT
    assert after["fx"][3] == "{}"
    assert after["marker"] == (str(compaction.FX_CLEAN_VERSION),)
    assert os.path.getsize(db) < os.path.getsize(glob.glob(db + ".bak-v5-*")[0])

    # 2 回目は何もしない (冪等)
    size = os.path.getsize(db)
    ensure_healthy_db(db)
    assert os.path.getsize(db) == size
    assert len(glob.glob(db + ".bak-v5-*")) == 1
    assert _snapshot(db) == after


def test_clean_features_extra_is_marked_without_rebuild(tmp_path):
    db = str(tmp_path / "clean-fx.duckdb")
    _make_v5_db(db, [_CLEAN_FX_TEXT, json.dumps(_KEPT_FX), "{}"])
    before = _snapshot(db)
    assert before["marker"] is None

    ensure_healthy_db(db)

    after = _snapshot(db)
    assert glob.glob(db + ".bak-v5-*") == []
    assert after["marker"] == (str(compaction.FX_CLEAN_VERSION),)
    assert {k: v for k, v in after.items() if k != "marker"} == {
        k: v for k, v in before.items() if k != "marker"
    }


def test_marked_database_is_not_scanned_again(tmp_path, monkeypatch):
    db = str(tmp_path / "marked.duckdb")
    _make_v5_db(db, ["{}"])
    ensure_healthy_db(db)  # 印を付ける

    # 印のあとに書かれた旧形式の行は、印の版が上がるまで再構築の対象にしない
    con = duckdb.connect(db)
    con.execute("UPDATE track_analyses SET features_extra_json = ? WHERE track_id = 1", [json.dumps(_LEGACY_FX)])
    con.close()
    ensure_healthy_db(db)
    assert glob.glob(db + ".bak-v5-*") == []

    monkeypatch.setattr(compaction, "FX_CLEAN_VERSION", compaction.FX_CLEAN_VERSION + 1)
    ensure_healthy_db(db)
    assert len(glob.glob(db + ".bak-v5-*")) == 1
    after = _snapshot(db)
    assert json.loads(after["fx"][1]) == _KEPT_FX
    assert after["marker"] == (str(compaction.FX_CLEAN_VERSION),)


def test_bloat_compaction_also_applies_allow_list(tmp_path, monkeypatch):
    db = str(tmp_path / "bloated.duckdb")
    _make_v5_db(db, [json.dumps(_LEGACY_FX)])
    con = duckdb.connect(db)
    con.execute(
        "INSERT INTO schema_info VALUES (?, ?)", [compaction.FX_CLEAN_KEY, str(compaction.FX_CLEAN_VERSION)]
    )
    con.close()
    monkeypatch.setattr(compaction, "COMPACT_MIN_BYTES", 1)
    monkeypatch.setattr(compaction, "COMPACT_BYTES_PER_TRACK", 1)
    monkeypatch.setattr(compaction, "COMPACT_RATIO_LIMIT", 1)

    ensure_healthy_db(db)

    assert len(glob.glob(db + ".bak-v5-*")) == 1
    assert json.loads(_snapshot(db)["fx"][1]) == _KEPT_FX


def test_unreadable_features_extra_is_kept_as_is(tmp_path):
    db = str(tmp_path / "odd-fx.duckdb")
    _make_v5_db(db, [json.dumps(_LEGACY_FX), "not json", "[1, 2]"])

    ensure_healthy_db(db)

    fx = _snapshot(db)["fx"]
    assert json.loads(fx[1]) == _KEPT_FX
    assert fx[2] == "not json"
    assert fx[3] == "[1, 2]"
