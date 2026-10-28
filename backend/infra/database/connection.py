from sqlmodel import create_engine, Session, text, select
from sqlalchemy.pool import QueuePool
from sqlalchemy import event
import os
import threading
import sys
from contextlib import contextmanager
from config import settings
from infra.database.schema import init_raw_db
from infra.database.compaction import ensure_healthy_db

# DBパス設定
DB_PATH = settings.DB_PATH
os.makedirs(os.path.dirname(DB_PATH), exist_ok=True)

DATABASE_URL = f"duckdb:///{DB_PATH}"

# Track actual pooled leases: FastAPI dependency enter/exit can run on
# different threads, so a thread-owned RLock around yield is not safe.
_lease_lock = threading.RLock()
_lease_count = 0
_maintenance_owner = None


@contextmanager
def exclusive_database():
    global _maintenance_owner
    with _lease_lock:
        if _maintenance_owner is not None or _lease_count:
            raise ValueError("データベースを使用中です。処理の完了後に再試行してください")
        for module_name, instance_name in (
            ("app.services.analysis_job_service", "analysis_job_service"),
            ("app.services.ingestion_app_service", "ingestion_app_service"),
        ):
            service = getattr(sys.modules.get(module_name), instance_name, None)
            if service is not None and service.is_running:
                raise ValueError("解析・取り込みを停止してからバックアップ／復元してください")
        _maintenance_owner = threading.get_ident()
    try:
        yield
    finally:
        with _lease_lock:
            _maintenance_owner = None


@contextmanager
def background_analysis_paused():
    """Let backup/restore run while the automatic embedding job is active.

    Enter this before database_activity/db_lock: the job thread needs those
    locks to return its track to the queue before it stops.
    """
    service = getattr(sys.modules.get("app.services.analysis_job_service"), "analysis_job_service", None)
    if service is None:
        yield
        return
    service.suspend_background()
    try:
        yield
    finally:
        service.resume_background()


# エンジン初期化 (設定を固定)
connect_args = {'config': {'worker_threads': 4, 'access_mode': 'READ_WRITE'}}
def create_library_engine(database_url):
    # DuckDB connections must not race while opening/closing the same file.
    # Keep bounded persistent connections instead of closing on every request.
    # Multiple leases keep a slow metadata request from blocking all searches.
    db_engine = create_engine(
        database_url,
        poolclass=QueuePool,
        pool_size=8,
        max_overflow=0,
        pool_timeout=120,
        connect_args=connect_args,
    )
    open_lock = threading.Lock()

    @event.listens_for(db_engine, "do_connect")
    def serialized_connect(dialect, record, args, kwargs):
        # QueuePool can create its initial connections from several threads.
        # Serialize that lifecycle step, not SQL execution or audio inference.
        global _lease_count
        with _lease_lock, open_lock:
            if _maintenance_owner is not None and _maintenance_owner != threading.get_ident():
                raise ValueError("バックアップ／復元中です。完了後に再試行してください")
            connection = dialect.connect(*args, **kwargs)
            # Count creation too: a newly opened handle exists before checkout.
            _lease_count += 1
            record.info["library_lease"] = True
            return connection

    @event.listens_for(db_engine, "checkout")
    def checkout(connection, record, proxy):
        global _lease_count
        with _lease_lock:
            if _maintenance_owner is not None and _maintenance_owner != threading.get_ident():
                raise ValueError("バックアップ／復元中です。完了後に再試行してください")
            if not record.info.get("library_lease"):
                _lease_count += 1
                record.info["library_lease"] = True

    @event.listens_for(db_engine, "checkin")
    def checkin(connection, record):
        global _lease_count
        with _lease_lock:
            if record.info.pop("library_lease", False):
                _lease_count -= 1

    return db_engine


engine = create_library_engine(DATABASE_URL)

db_lock = threading.RLock()
# Workflow workers and SQLite queue operations share this gate. Pooled
# request connections are tracked separately by exclusive_database().
database_activity = threading.RLock()

def init_db():
    """
    アプリケーション起動時のDB初期化フロー。
    Raw SQL + マイグレーション機能でスキーマを管理。
    """
    from utils.seeding import seed_initial_data

    with db_lock:
        try:
            # 0. SQLAlchemy エンジンが接続する前に、スキーマ移行(v4: BLOB化)や
            #    肥大ファイルのコンパクションが必要ならファイルを再構築する。
            from pathlib import Path
            from infra.database.restore_recovery import recover_pending_restore
            recover_pending_restore(Path(DB_PATH))
            ensure_healthy_db(DB_PATH)

            # 1. Raw SQL によるテーブル作成 + マイグレーション実行
            init_raw_db(engine)
            
            # 2. 初期データの投入
            with Session(engine) as session:
                seed_initial_data(session)
                # v0.4+: model/API credentials are owned by the MCP client.
                # Remove credentials previously stored by plumdeck.
                from app.services.setting_app_service import is_retired_llm_setting_key
                from domain.models.setting import Setting
                for saved_setting in session.exec(select(Setting)).all():
                    if is_retired_llm_setting_key(saved_setting.key):
                        session.delete(saved_setting)
                session.commit()
                
        except Exception as e:
            print(f"Error during database initialization: {e}")
            raise e

def checkpoint_db():
    """
    WAL を本体ファイルへ畳み込む。正常終了時に呼び、肥大の抑制を助ける。
    (Tauri が SIGKILL する場合は動かないため、あくまで補助。)
    """
    try:
        with db_lock:
            with engine.connect() as conn:
                conn.execute(text("CHECKPOINT"))
    except Exception as e:
        print(f"DEBUG: CHECKPOINT skipped: {e}")


def close_db():
    """
    データベース接続を終了する。
    main.py の lifespan イベントから呼び出されます。
    """
    engine.dispose()


def reopen_db(db_path: str | None = None):
    """Recreate the pooled engine after an atomic restore replaces the DB file."""
    global engine, DB_PATH, DATABASE_URL
    with db_lock:
        engine.dispose()
        if db_path is not None:
            DB_PATH = db_path
        DATABASE_URL = f"duckdb:///{DB_PATH}"
        # Keep the engine object used by already imported repositories. dispose()
        # replaces its pool; replacing the object leaves stale engine references.
        if str(engine.url) != DATABASE_URL:
            engine = create_library_engine(DATABASE_URL)
        init_raw_db(engine)
    return engine

def get_session():
    with Session(engine) as session:
        yield session

def get_setting_value(session: Session, key: str, default: str = "") -> str:
    from domain.models.setting import Setting
    try:
        setting = session.get(Setting, key)
        if setting:
            return setting.value
    except Exception as e:
        print(f"DEBUG: Error getting setting '{key}': {e}")
    return default

def set_setting_value(session: Session, key: str, value: str):
    from domain.models.setting import Setting
    try:
        setting = session.get(Setting, key)
        if not setting:
            setting = Setting(key=key, value=value)
            session.add(setting)
        else:
            setting.value = value
            session.add(setting)
        
        session.commit()
        session.refresh(setting)
        return setting
    except Exception as e:
        print(f"DEBUG: Error setting value for '{key}': {e}")
        session.rollback()
        return None
