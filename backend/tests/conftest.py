import os
import importlib.util
import pytest
import sys
import tempfile
import uuid
from typing import Generator
from sqlmodel import Session, create_engine

# 1. パス解決: backendディレクトリをsys.pathに追加
CURRENT_DIR = os.path.dirname(os.path.abspath(__file__))
BACKEND_DIR = os.path.dirname(CURRENT_DIR)
if BACKEND_DIR not in sys.path:
    sys.path.insert(0, BACKEND_DIR)

import infra.database.connection as db_connection
from infra.database.schema import init_raw_db
from utils.seeding import seed_initial_data

@pytest.fixture(name="session", scope="function")
def session_fixture(mocker) -> Generator[Session, None, None]:
    """
    テストごとに完全に独立したDB環境（物理ファイル）を構築する。
    """
    
    # ユニークなDBファイルパスを生成
    unique_id = str(uuid.uuid4())
    test_db_path = os.path.join(tempfile.gettempdir(), f"plumdeck_test_{unique_id}.duckdb")
    
    # アプリケーションが参照する環境変数を上書き
    os.environ["DB_PATH"] = test_db_path
    
    # テスト用エンジンの作成 (設定を固定)
    connect_args = {'config': {'worker_threads': 4, 'access_mode': 'READ_WRITE'}}
    engine = create_engine(
        f"duckdb:///{test_db_path}",
        connect_args=connect_args
    )

    # アプリケーション全体で使用されるエンジングローバル変数をテスト用に差し替え
    db_connection.engine = engine
    db_connection.DB_PATH = test_db_path
    db_connection.DATABASE_URL = f"duckdb:///{test_db_path}"

    # 1. Raw SQLでテーブル作成 + マイグレーション実行
    init_raw_db(engine)

    # 2. アプリ起動時の init_db がテスト中に走って競合しないようモック化
    mocker.patch("infra.database.connection.init_db")

    # 3. 初期データの投入 (現在は初期データなし、互換性のため呼び出しのみ維持)
    with Session(engine) as s:
        seed_initial_data(s)

    # テスト実行用のセッションを提供
    with Session(engine) as session:
        yield session

    # テスト終了後のクリーンアップ
    engine.dispose()
    if os.path.exists(test_db_path):
        try:
            os.remove(test_db_path)
        except OSError:
            pass

@pytest.fixture(name="client")
def client_fixture(session: Session) -> Generator:
    """FastAPIのTestClientを提供し、DBセッションをDIで差し替える"""
    from fastapi.testclient import TestClient
    from main import app
    from infra.database.connection import get_session

    def get_session_override():
        return session

    app.dependency_overrides[get_session] = get_session_override
    with TestClient(app) as client:
        try:
            yield client
        finally:
            # This fixture shares a session across requests; production closes
            # each request's session. End test reads before shutdown CHECKPOINT.
            session.rollback()
    app.dependency_overrides.clear()

@pytest.fixture(autouse=True)
def mock_external_deps(mocker):
    """
    外部ライブラリをグローバルにモック化する。
    """
    # Non-audio service tests also run where Essentia is not installed.
    if importlib.util.find_spec("essentia") is not None:
        for algorithm in ("MonoLoader", "RhythmExtractor2013", "KeyExtractor", "RMS",
                          "Danceability", "SpectralCentroidTime", "ZeroCrossingRate",
                          "Spectrum", "Windowing", "RollOff", "Flux",
                          "TensorflowPredictMusiCNN", "LoudnessEBUR128"):
            mocker.patch(f"essentia.standard.{algorithm}", create=True)

    # メタデータ抽出ライブラリのモック
    mocker.patch("tinytag.TinyTag.get")
    mocker.patch("mutagen.File")
    mocker.patch("mutagen.id3.ID3")
    mocker.patch("mutagen.mp4.MP4")
    mocker.patch("mutagen.flac.FLAC")

@pytest.fixture(autouse=True)
def no_real_rekordbox_history(mocker):
    """Never let a test read the developer's own rekordbox HISTORY."""
    from infra import rekordbox_library
    mocker.patch.object(rekordbox_library, "play_sessions", return_value=())


@pytest.fixture(autouse=True)
def no_real_rekordbox_values(mocker):
    """Saves read rekordbox BPM/key; by default the collection is unreadable,
    as on a machine without rekordbox. Tests patch it to model a library."""
    from infra import rekordbox_library
    mocker.patch.object(rekordbox_library, "collection_index",
                        side_effect=rekordbox_library.RekordboxLibraryUnavailable("not in tests"))


@pytest.fixture(autouse=True)
def no_automatic_embedding_jobs(monkeypatch):
    """Imports queue background embeddings; tests that need the queue build
    their own AnalysisJobService instead of starting native workers here."""
    from app.services.analysis_job_service import analysis_job_service
    monkeypatch.setattr(analysis_job_service, "enqueue_embedding", lambda **kwargs: {"status": "idle"})
