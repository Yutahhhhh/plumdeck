"""
起動時の DuckDB ファイル健全性チェック / 再構築。

背景:
- 波形(2000点)や埋め込みを JSON テキストで行に保存し、再解析のたびに UPDATE していたため
  DuckDB ファイルが 19GB まで肥大化し、起動時の DB オープンに数分かかっていた。
- DuckDB は UPDATE/DELETE で空いた領域を OS に返さない (VACUUM でも縮まない)。

このモジュールは SQLAlchemy エンジンが接続する前 (init_db の冒頭) に呼ばれ、
必要なら「新しいファイルへ生きているデータだけを書き出して差し替える」再構築を行う:

1. schema version < CURRENT_SCHEMA_VERSION  -> 新スキーマ(BLOB化)へ変換しつつ再構築
2. features_extra_json に許可リスト外のキーが残っている -> 再構築 (1 回だけ)
3. version は最新だがファイルが肥大  -> 変換なしで再構築 (コンパクション)

どの再構築でも features_extra_json は utils/features_extra の許可リストで作り直す
(読み手のない拍・波形の重複配列や記録用キーを落とす)。UPDATE で書き換えると
DuckDB はファイルを大きくするため、再構築の中でだけ行う。

再構築は新ファイルに書き出してから rename で差し替えるため、成功するまで元ファイルは無傷。
元ファイルは `<db>.bak-v<N>-<timestamp>` として残す (自動削除しない)。
"""
import json
import os
import time
from typing import List, Optional

import duckdb

from infra.database.schema import (
    CURRENT_SCHEMA_VERSION,
    PLAIN_TABLES,
    SEQUENCES,
    get_schema_statements,
)
from utils.array_codec import pack_f32, pack_u8_waveform
from utils.features_extra import PERSISTED_KEYS, persisted_features_extra
from utils.logger import get_logger

logger = get_logger(__name__)

# 肥大判定のしきい値
COMPACT_MIN_BYTES = 256 * 1024 * 1024          # これ未満のファイルは対象外
COMPACT_BYTES_PER_TRACK = 64 * 1024            # 1曲あたりの許容目安 (BLOB化後は十分過大)
COMPACT_RATIO_LIMIT = 4                        # 目安の何倍を超えたら再構築するか
BATCH = 500

# schema_info key marking that features_extra_json holds only PERSISTED_KEYS.
# Bump FX_CLEAN_VERSION when PERSISTED_KEYS shrinks so existing rows are rebuilt.
FX_CLEAN_KEY = "features_extra_clean"
FX_CLEAN_VERSION = 1
_PERSISTED = frozenset(PERSISTED_KEYS)


def ensure_healthy_db(db_path: str) -> None:
    if not db_path or not os.path.exists(db_path):
        return  # 新規インストール: init_raw_db が新スキーマで作成する

    try:
        con = duckdb.connect(db_path)  # read-write: 保留中の WAL はここで自動リプレイされる
    except Exception as e:
        # 別プロセスがロック中など。壊れた状態で起動を続けるより、ここで明示的に失敗させる。
        raise RuntimeError(f"起動時の DB チェックでファイルを開けませんでした: {e}") from e

    new_path = db_path + ".rebuild"
    built = False
    try:
        version = _read_version(con)
        legacy = _has_legacy_analysis_columns(con)
        needs_migrate = version < CURRENT_SCHEMA_VERSION or legacy
        needs_fx = (not needs_migrate) and _needs_fx_cleanup(con)
        needs_compact = (not needs_migrate) and (not needs_fx) and _is_bloated(con, db_path)

        if not needs_migrate and not needs_fx and not needs_compact:
            return

        size_mb = os.path.getsize(db_path) / 1e6
        if needs_migrate:
            reason = f"schema v{version} -> v{CURRENT_SCHEMA_VERSION}"
        elif needs_fx:
            reason = "features_extra の許可リスト外キーの削除"
        else:
            reason = "肥大ファイルのコンパクション"
        logger.warning(f"DuckDB ファイルを再構築します ({reason}, 現在 {size_mb:.0f}MB)")

        if os.path.exists(new_path):
            os.remove(new_path)
        _build_new_file(con, new_path, convert=legacy)
        built = True
    finally:
        con.close()
        if not built and os.path.exists(new_path):
            try:
                os.remove(new_path)
            except OSError:
                pass

    _swap(db_path, new_path)


# --------------------------------------------------------------------------- #
# 判定ヘルパー
# --------------------------------------------------------------------------- #
def _read_info_int(con, key: str) -> int:
    try:
        row = con.execute("SELECT value FROM schema_info WHERE key = ?", [key]).fetchone()
        return int(row[0]) if row and row[0] is not None else 0
    except Exception:
        return 0


def _read_version(con) -> int:
    return _read_info_int(con, "version")


def _write_info(con, key: str, value: str) -> None:
    con.execute(
        "INSERT INTO schema_info (key, value) VALUES (?, ?) "
        "ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )


def _needs_fx_cleanup(con) -> bool:
    """features_extra_json に許可リスト外のキーを持つ行があるか。

    中身を走査するのは印 (FX_CLEAN_KEY) が無いときだけ。きれいならその場で印を付け、
    以後の起動は印だけを見る。古いバックアップを復元すると印が無いので再び判定される。
    """
    if _read_info_int(con, FX_CLEAN_KEY) >= FX_CLEAN_VERSION:
        return False
    try:
        dirty = con.execute(
            "SELECT 1 FROM track_analyses WHERE CASE "
            "WHEN json_valid(features_extra_json) AND json_type(features_extra_json) = 'OBJECT' "
            "THEN NOT list_has_all(?::VARCHAR[], json_keys(features_extra_json)) "
            "ELSE false END LIMIT 1",
            [list(PERSISTED_KEYS)],
        ).fetchone() is not None
        if not dirty:
            _write_info(con, FX_CLEAN_KEY, str(FX_CLEAN_VERSION))
    except Exception as e:
        # 判定できなければ起動を止めず、次回の起動で再判定する。
        logger.warning(f"features_extra の判定をスキップしました: {e}")
        return False
    return dirty


def _has_legacy_analysis_columns(con) -> bool:
    """track_analyses に旧 JSON カラムが残っているか。"""
    try:
        cols = {
            r[0]
            for r in con.execute(
                "SELECT column_name FROM information_schema.columns "
                "WHERE table_schema = 'main' AND table_name = 'track_analyses'"
            ).fetchall()
        }
    except Exception:
        return False
    return "waveform_peaks" in cols or "beat_positions" in cols


def _is_bloated(con, db_path: str) -> bool:
    size = os.path.getsize(db_path)
    if size < COMPACT_MIN_BYTES:
        return False
    try:
        n = con.execute("SELECT count(*) FROM tracks").fetchone()[0] or 0
    except Exception:
        n = 0
    budget = max(int(n), 1) * COMPACT_BYTES_PER_TRACK * COMPACT_RATIO_LIMIT
    return size > budget


# --------------------------------------------------------------------------- #
# 再構築
# --------------------------------------------------------------------------- #
def _column_names(con, table: str) -> List[str]:
    return [
        r[0]
        for r in con.execute(
            "SELECT column_name FROM information_schema.columns "
            "WHERE table_schema = 'main' AND table_name = ? ORDER BY ordinal_position",
            [table],
        ).fetchall()
    ]


def _common_columns(src, dst, table: str) -> List[str]:
    src_cols = set(_column_names(src, table))
    return [c for c in _column_names(dst, table) if c in src_cols]


def _table_exists(con, table: str) -> bool:
    row = con.execute(
        "SELECT 1 FROM information_schema.tables "
        "WHERE table_schema = 'main' AND table_name = ?",
        [table],
    ).fetchone()
    return row is not None


def _copy_plain_table(src, dst, table: str) -> int:
    cols = _common_columns(src, dst, table)
    if not cols:
        return 0
    col_list = ", ".join(f'"{c}"' for c in cols)
    placeholders = ", ".join(["?"] * len(cols))
    rows = src.execute(f"SELECT {col_list} FROM {table}").fetchall()
    if rows:
        dst.executemany(
            f"INSERT INTO {table} ({col_list}) VALUES ({placeholders})", rows
        )
    return len(rows)


def _as_list(v) -> list:
    if v is None:
        return []
    if isinstance(v, (list, tuple)):
        return list(v)
    if isinstance(v, (bytes, bytearray)):
        v = v.decode("utf-8", "ignore")
    if isinstance(v, str):
        s = v.strip()
        if not s:
            return []
        try:
            parsed = json.loads(s)
            return parsed if isinstance(parsed, list) else []
        except Exception:
            return []
    return []


def _clean_features_extra(fx) -> str:
    """Rebuild a features_extra_json object with the persisted allow-list.

    Already clean or non-object values are kept byte-for-byte.
    """
    if not fx:
        return "{}"
    try:
        extra = json.loads(fx)
    except (TypeError, ValueError):
        return fx
    if not isinstance(extra, dict) or extra.keys() <= _PERSISTED:
        return fx
    return json.dumps(persisted_features_extra(extra))


def _copy_track_analyses(src, dst, convert: bool) -> None:
    total = src.execute("SELECT count(*) FROM track_analyses").fetchone()[0] or 0
    if convert:
        cur = src.execute(
            "SELECT track_id, beat_positions, waveform_peaks, features_extra_json "
            "FROM track_analyses"
        )
    else:
        cur = src.execute(
            "SELECT track_id, beats_f32, waveform_u8, features_extra_json "
            "FROM track_analyses"
        )
    done = 0
    while True:
        chunk = cur.fetchmany(BATCH)
        if not chunk:
            break
        out = []
        for track_id, beats, wave, fx in chunk:
            if convert:
                out.append(
                    (
                        track_id,
                        pack_f32(_as_list(beats)),
                        pack_u8_waveform(_as_list(wave)),
                        _clean_features_extra(fx),
                    )
                )
            else:
                out.append((track_id, beats, wave, _clean_features_extra(fx)))
        dst.executemany(
            "INSERT INTO track_analyses "
            "(track_id, beats_f32, waveform_u8, features_extra_json) VALUES (?, ?, ?, ?)",
            out,
        )
        done += len(chunk)
        logger.info(f"Rebuilding DB: {done}/{total} analyses")


def _build_new_file(src, new_path: str, convert: bool) -> None:
    # 既存 id の続きから採番するためシーケンス開始値を決める
    seq_starts = {}
    for seq_name, table in SEQUENCES.items():
        try:
            mx = src.execute(f"SELECT COALESCE(MAX(id), 0) FROM {table}").fetchone()[0]
        except Exception:
            mx = 0
        seq_starts[seq_name] = int(mx or 0) + 1

    dst = duckdb.connect(new_path)
    try:
        for stmt in get_schema_statements(seq_starts):
            dst.execute(stmt)

        for table in PLAIN_TABLES:
            if _table_exists(src, table):
                n = _copy_plain_table(src, dst, table)
                logger.info(f"Rebuilding DB: copied {n} rows from {table}")

        if _table_exists(src, "track_embeddings"):
            n = _copy_plain_table(src, dst, "track_embeddings")
            logger.info(f"Rebuilding DB: copied {n} rows from track_embeddings")

        if _table_exists(src, "track_analyses"):
            _copy_track_analyses(src, dst, convert)

        _write_info(dst, "version", str(CURRENT_SCHEMA_VERSION))
        _write_info(dst, FX_CLEAN_KEY, str(FX_CLEAN_VERSION))
        dst.execute("CHECKPOINT")
    finally:
        dst.close()


def _swap(db_path: str, new_path: str) -> None:
    ts = time.strftime("%Y%m%d-%H%M%S")
    backup = f"{db_path}.bak-v{CURRENT_SCHEMA_VERSION}-{ts}"
    os.replace(db_path, backup)
    src_wal = db_path + ".wal"
    if os.path.exists(src_wal):
        try:
            os.replace(src_wal, backup + ".wal")
        except OSError:
            pass
    os.replace(new_path, db_path)
    new_size_mb = os.path.getsize(db_path) / 1e6
    logger.warning(
        f"DuckDB 再構築完了: 新ファイル {new_size_mb:.0f}MB。旧ファイルは {backup} に保存しました。"
    )
