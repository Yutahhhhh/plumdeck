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
差し替えは「元ファイルをハードリンク(またはコピー)で `<db>.bak-v<N>-<timestamp>` に残す ->
新ファイルを os.replace で上書き」の順で行い、どの時点で落ちても db_path は必ず存在する。
旧版の差し替え (先に元ファイルを rename) の途中で落ちた場合は、起動時に .bak から戻す。
バックアップは BACKUP_KEEP_DAYS を過ぎたら、正常に開けた起動時に削除する。

許可リストの適用 (2) と肥大のコンパクション (3) は最適化なので、失敗しても元ファイルの
まま起動を続け、次回の起動で再試行する。起動を止めるのはスキーマ移行 (1) の失敗だけ。
"""
import glob
import json
import os
import shutil
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
# The new file is attached to the source connection under this name so tables
# are copied inside DuckDB (row-by-row executemany took minutes on 25k tracks).
_DST = "plumdeck_rebuild"
_FX_UDF = "plumdeck_clean_features_extra"

# schema_info key marking that features_extra_json holds only PERSISTED_KEYS.
# Bump FX_CLEAN_VERSION when PERSISTED_KEYS shrinks so existing rows are rebuilt.
FX_CLEAN_KEY = "features_extra_clean"
FX_CLEAN_VERSION = 1
_PERSISTED = frozenset(PERSISTED_KEYS)

BACKUP_KEEP_DAYS = 30
_REPLACE_ATTEMPTS = 5  # Windows: Defender or an indexer may hold the new file briefly


def ensure_healthy_db(db_path: str, keep_backup: bool = True) -> None:
    """Rebuild the file when needed. ``keep_backup=False`` skips the .bak copy
    (restore staging, whose archive is the backup)."""
    if not db_path:
        return
    _recover_interrupted_swap(db_path)
    if not os.path.exists(db_path):
        return  # 新規インストール: init_raw_db が新スキーマで作成する

    try:
        con = duckdb.connect(db_path)  # read-write: 保留中の WAL はここで自動リプレイされる
    except Exception as e:
        # 別プロセスがロック中など。壊れた状態で起動を続けるより、ここで明示的に失敗させる。
        raise RuntimeError(f"起動時の DB チェックでファイルを開けませんでした: {e}") from e

    new_path = db_path + ".rebuild"
    built = False
    needs_migrate = False
    try:
        version = _read_version(con)
        legacy = _has_legacy_analysis_columns(con)
        needs_migrate = version < CURRENT_SCHEMA_VERSION or legacy
        needs_fx = (not needs_migrate) and _needs_fx_cleanup(con)
        needs_compact = (not needs_migrate) and (not needs_fx) and _is_bloated(con, db_path)

        if not needs_migrate and not needs_fx and not needs_compact:
            _prune_backups(db_path)
            return

        size_mb = os.path.getsize(db_path) / 1e6
        if needs_migrate:
            reason = f"schema v{version} -> v{CURRENT_SCHEMA_VERSION}"
        elif needs_fx:
            reason = "features_extra の許可リスト外キーの削除"
        else:
            reason = "肥大ファイルのコンパクション"
        logger.warning(f"DuckDB ファイルを再構築します ({reason}, 現在 {size_mb:.0f}MB)")

        _remove_partial(new_path)
        try:
            _build_new_file(con, new_path, convert=legacy)
        except Exception as e:
            if needs_migrate:
                raise
            # Optimization only: keep the original file and retry on next start.
            logger.warning(f"DuckDB の再構築をスキップしました (次回の起動で再試行します): {e}")
            return
        con.execute("CHECKPOINT")  # leave no WAL that belongs to the old file
        built = True
    finally:
        con.close()
        if not built:
            try:
                _remove_partial(new_path)
            except OSError:
                pass

    try:
        _swap(db_path, new_path, keep_backup)
    except Exception as e:
        try:
            _remove_partial(new_path)
        except OSError:
            pass
        if needs_migrate:
            raise
        logger.warning(f"DuckDB の差し替えをスキップしました (次回の起動で再試行します): {e}")
        return
    if keep_backup:
        _prune_backups(db_path)


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


def _write_info(con, key: str, value: str, table: str = "schema_info") -> None:
    con.execute(
        f"INSERT INTO {table} (key, value) VALUES (?, ?) "
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
def _remove_partial(new_path: str) -> None:
    # Leftovers of an interrupted rebuild, including its WAL.
    for path in (new_path, new_path + ".wal"):
        if os.path.exists(path):
            os.remove(path)


def _ident(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def _literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _column_names(con, catalog: str, table: str) -> List[str]:
    return [
        r[0]
        for r in con.execute(
            "SELECT column_name FROM information_schema.columns "
            "WHERE table_catalog = ? AND table_schema = 'main' AND table_name = ? "
            "ORDER BY ordinal_position",
            [catalog, table],
        ).fetchall()
    ]


def _table_exists(con, catalog: str, table: str) -> bool:
    row = con.execute(
        "SELECT 1 FROM information_schema.tables "
        "WHERE table_catalog = ? AND table_schema = 'main' AND table_name = ?",
        [catalog, table],
    ).fetchone()
    return row is not None


def _copy_plain_table(con, src_db: str, table: str) -> int:
    src_cols = set(_column_names(con, src_db, table))
    cols = [c for c in _column_names(con, _DST, table) if c in src_cols]
    if not cols:
        return 0
    col_list = ", ".join(_ident(c) for c in cols)
    return con.execute(
        f"INSERT INTO {_DST}.main.{_ident(table)} ({col_list}) "
        f"SELECT {col_list} FROM {_ident(src_db)}.main.{_ident(table)}"
    ).fetchone()[0]


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


def _copy_track_analyses(con, src_db: str, convert: bool) -> None:
    src_table = f"{_ident(src_db)}.main.track_analyses"
    insert = (
        f"INSERT INTO {_DST}.main.track_analyses "
        "(track_id, beats_f32, waveform_u8, features_extra_json) "
    )
    if not convert:
        con.create_function(
            _FX_UDF, _clean_features_extra, ["VARCHAR"], "VARCHAR",
            null_handling="special", side_effects=False,
        )
        try:
            n = con.execute(
                insert + f"SELECT track_id, beats_f32, waveform_u8, {_FX_UDF}(features_extra_json) "
                f"FROM {src_table}"
            ).fetchone()[0]
        finally:
            con.remove_function(_FX_UDF)
        logger.info(f"Rebuilding DB: copied {n} analyses")
        return

    # v3 の JSON 配列は Python で BLOB に詰め直す
    total = con.execute(f"SELECT count(*) FROM {src_table}").fetchone()[0] or 0
    cur = con.cursor()  # 読み出しを続けたまま同じ DB インスタンスへ書き込む
    try:
        cur.execute(
            "SELECT track_id, beat_positions, waveform_peaks, features_extra_json "
            f"FROM {src_table}"
        )
        done = 0
        while True:
            chunk = cur.fetchmany(BATCH)
            if not chunk:
                break
            out = [
                (
                    track_id,
                    pack_f32(_as_list(beats)),
                    pack_u8_waveform(_as_list(wave)),
                    _clean_features_extra(fx),
                )
                for track_id, beats, wave, fx in chunk
            ]
            con.executemany(insert + "VALUES (?, ?, ?, ?)", out)
            done += len(chunk)
            logger.info(f"Rebuilding DB: {done}/{total} analyses")
    finally:
        cur.close()


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
    finally:
        dst.close()

    src_db = src.execute("SELECT current_database()").fetchone()[0]
    src.execute(f"ATTACH {_literal(new_path)} AS {_DST}")
    try:
        for table in PLAIN_TABLES + ["track_embeddings"]:
            if _table_exists(src, src_db, table):
                n = _copy_plain_table(src, src_db, table)
                logger.info(f"Rebuilding DB: copied {n} rows from {table}")

        if _table_exists(src, src_db, "track_analyses"):
            _copy_track_analyses(src, src_db, convert)

        info = f"{_DST}.main.schema_info"
        _write_info(src, "version", str(CURRENT_SCHEMA_VERSION), table=info)
        _write_info(src, FX_CLEAN_KEY, str(FX_CLEAN_VERSION), table=info)
        src.execute(f"CHECKPOINT {_DST}")
    finally:
        try:
            src.execute(f"DETACH {_DST}")
        except Exception:
            pass  # the caller's con.close() releases the file as well


def _backup_path(db_path: str) -> str:
    return f"{db_path}.bak-v{CURRENT_SCHEMA_VERSION}-{time.strftime('%Y%m%d-%H%M%S')}"


def _keep_copy(source: str, target: str) -> None:
    """Hard link (no extra space while both names exist), else a full copy."""
    try:
        os.link(source, target)
    except OSError:
        shutil.copy2(source, target)


def _replace(source: str, target: str) -> None:
    # os.replace overwrites atomically on POSIX and on Windows (MoveFileEx
    # REPLACE_EXISTING), so the target name never disappears.
    for attempt in range(_REPLACE_ATTEMPTS):
        try:
            os.replace(source, target)
            return
        except PermissionError:
            if attempt == _REPLACE_ATTEMPTS - 1:
                raise
            time.sleep(0.2 * (attempt + 1))


def _swap(db_path: str, new_path: str, keep_backup: bool = True) -> None:
    backup = _backup_path(db_path) if keep_backup else None
    if backup:
        _keep_copy(db_path, backup)
    src_wal = db_path + ".wal"
    wal_aside = (backup or db_path + ".stale") + ".wal"
    try:
        if os.path.exists(src_wal):
            # The checkpoint before close normally leaves none; a stale WAL must
            # never be replayed onto the new file.
            os.replace(src_wal, wal_aside)
        _replace(new_path, db_path)
    except Exception:
        if os.path.exists(wal_aside) and not os.path.exists(src_wal):
            os.replace(wal_aside, src_wal)
        if backup and os.path.exists(backup):
            os.remove(backup)
        raise
    if not backup and os.path.exists(wal_aside):
        os.remove(wal_aside)
    new_size_mb = os.path.getsize(db_path) / 1e6
    if backup:
        logger.warning(
            f"DuckDB 再構築完了: 新ファイル {new_size_mb:.0f}MB。旧ファイルは {backup} に保存しました"
            f" (動作を確認したら削除できます。{BACKUP_KEEP_DAYS} 日後の起動時に自動で削除します)。"
        )
    else:
        logger.info(f"DuckDB 再構築完了: 新ファイル {new_size_mb:.0f}MB")


def _backup_time(path: str) -> Optional[float]:
    """Creation time from the '.bak-v<N>-<YYYYmmdd-HHMMSS>' suffix (not mtime,
    which a hard link shares with the original)."""
    try:
        return time.mktime(time.strptime(path.rsplit("-", 2)[-2] + path.rsplit("-", 1)[-1], "%Y%m%d%H%M%S"))
    except (ValueError, IndexError):
        return None


def _backups(db_path: str) -> List[str]:
    return sorted(path for path in glob.glob(glob.escape(db_path) + ".bak-v*")
                  if not path.endswith(".wal"))


def _recover_interrupted_swap(db_path: str) -> None:
    """Undo a swap that died after moving the live file away (older releases).

    Only when the rebuild file is still there: a missing DB alone may be a
    deliberate reset, and the newest backup is the untouched original.
    """
    new_path = db_path + ".rebuild"
    if os.path.exists(db_path) or not os.path.exists(new_path):
        return
    backups = _backups(db_path)
    if not backups:
        return
    backup = max(backups, key=lambda path: (_backup_time(path) or 0, path))
    logger.warning(f"中断した DuckDB の差し替えを戻します: {backup} -> {db_path}")
    if os.path.exists(backup + ".wal"):
        os.replace(backup + ".wal", db_path + ".wal")
    os.replace(backup, db_path)
    _remove_partial(new_path)


def _prune_backups(db_path: str) -> None:
    cutoff = time.time() - BACKUP_KEEP_DAYS * 86400
    for backup in _backups(db_path):
        created = _backup_time(backup)
        if created is None or created >= cutoff:
            continue
        try:
            for path in (backup, backup + ".wal"):
                if os.path.exists(path):
                    os.remove(path)
            logger.info(f"古い DuckDB バックアップを削除しました: {backup}")
        except OSError as e:
            logger.warning(f"DuckDB バックアップを削除できませんでした: {backup}: {e}")
