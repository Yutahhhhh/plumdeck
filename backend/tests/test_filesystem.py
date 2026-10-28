import os
import json
import pytest
from fastapi.testclient import TestClient
from sqlmodel import Session
from models import Track, TrackEmbedding

def test_stream_track(client: TestClient, session: Session, tmp_path):
    # ダミーの音声ファイルを作成
    audio_file = tmp_path / "test_audio.mp3"
    audio_file.write_bytes(b"fake audio content")
    
    # DBにトラックを登録
    track = Track(
        filepath=str(audio_file),
        title="Stream Test",
        artist="Artist",
        album="Album",
        genre="Genre",
        bpm=120,
        duration=60
    )
    session.add(track)
    session.commit()
    
    # ストリーミングAPIをコール
    # pathパラメータはURLエンコードが必要だが、TestClientが処理してくれるか確認
    # resolve_pathの挙動に依存するが、絶対パスならそのまま通るはず
    response = client.get("/api/stream", params={"path": str(audio_file)})
    assert response.status_code == 200
    assert response.content == b"fake audio content"

def test_stream_track_not_found(client: TestClient):
    response = client.get("/api/stream", params={"path": "/non/existent/file.mp3"})
    assert response.status_code == 404

def test_get_track_metadata(client: TestClient, session: Session, tmp_path):
    # ダミーファイル
    f = tmp_path / "meta.mp3"
    f.write_bytes(b"content")
    
    track = Track(
        filepath=str(f),
        title="Meta Title",
        artist="Meta Artist",
        album="Meta Album",
        genre="Meta Genre",
        bpm=120,
        duration=100
    )
    session.add(track)
    session.commit()
    
    # fs_service.get_track_metadata は内部でタグ情報を読む可能性があるため
    # 完全にテストするには tinytag をモックするか、実際のMP3を用意する必要がある
    # ここでは簡易的に 404 にならないこと、あるいはエラーハンドリングを確認する
    # 実際のファイルがMP3でないため、tinytagが失敗してNoneを返すか、
    # あるいはDBの情報だけで返す実装かによる。
    # filesystem.py を見ると fs_service.get_track_metadata を呼んでいる。
    
    # モックを使って fs_service の挙動を制御するのが安全
    # with pytest.raises(Exception): 
    #    pass

    # 今回は fs_service.get_track_metadata をモックする
    from app.services.filesystem_app_service import FilesystemAppService
    
    # MonkeyPatch
    original_method = FilesystemAppService.get_track_metadata
    
    def mock_get_metadata(self, track_id):
        if track_id == track.id:
            return {"title": "Mock Title", "artist": "Mock Artist"}
        return None
        
    FilesystemAppService.get_track_metadata = mock_get_metadata
    
    try:
        response = client.get("/api/metadata", params={"track_id": track.id})
        assert response.status_code == 200
        assert response.json()["title"] == "Mock Title"
        
        response = client.get("/api/metadata", params={"track_id": 9999})
        assert response.status_code == 404
    finally:
        FilesystemAppService.get_track_metadata = original_method

def test_list_directory(client: TestClient, session: Session, tmp_path):
    # ディレクトリ構造作成
    (tmp_path / "subdir").mkdir()
    (tmp_path / "file1.mp3").touch()
    
    # リクエスト
    response = client.post("/api/fs/list", json={"path": str(tmp_path)})
    assert response.status_code == 200
    data = response.json()
    
    filenames = [item["name"] for item in data]
    assert "subdir" in filenames
    assert "file1.mp3" in filenames


def test_list_directory_only_marks_completed_analysis_as_analyzed(
    client: TestClient, session: Session, tmp_path
):
    completed_file = tmp_path / "completed.mp3"
    incomplete_file = tmp_path / "incomplete.mp3"
    failed_file = tmp_path / "failed.mp3"
    for path in (completed_file, incomplete_file, failed_file):
        path.touch()

    completed = Track(
        filepath=str(completed_file),
        title="Completed",
        artist="Artist",
        album="Album",
        genre="Genre",
        bpm=120,
        duration=60,
    )
    # Fast imports can leave a valid Track row before audio analysis completes.
    incomplete = Track(
        filepath=str(incomplete_file),
        title="Incomplete",
        artist="Artist",
        album="Album",
        genre="Genre",
        bpm=None,
        duration=60,
    )
    # A partial result without an embedding must also remain retryable.
    failed = Track(
        filepath=str(failed_file),
        title="Failed",
        artist="Artist",
        album="Album",
        genre="Genre",
        bpm=120,
        duration=60,
    )
    session.add_all([completed, incomplete, failed])
    session.flush()
    session.add(
        TrackEmbedding(
            track_id=completed.id,
            embedding_json=json.dumps([0.1] * 200),
            model_name="test",
        )
    )
    session.commit()

    response = client.post(
        "/api/fs/list",
        json={"path": str(tmp_path), "hide_analyzed": False},
    )
    assert response.status_code == 200
    items = {item["name"]: item for item in response.json() if not item["is_dir"]}
    assert items["completed.mp3"]["is_analyzed"] is True
    assert items["incomplete.mp3"]["is_analyzed"] is False
    assert items["failed.mp3"]["is_analyzed"] is False

    hidden_response = client.post(
        "/api/fs/list",
        json={"path": str(tmp_path), "hide_analyzed": True},
    )
    assert hidden_response.status_code == 200
    visible_names = {item["name"] for item in hidden_response.json()}
    assert "completed.mp3" not in visible_names
    assert "incomplete.mp3" in visible_names
    assert "failed.mp3" in visible_names


def test_metadata_omits_beat_positions(client: TestClient, session: Session, tmp_path):
    from domain.models.track import TrackAnalysis
    audio = tmp_path / "song.mp3"
    audio.write_bytes(b"\0" * 64)
    track = Track(filepath=str(audio), title="T", artist="A", bpm=120, duration=10)
    session.add(track)
    session.flush()
    analysis = TrackAnalysis(track_id=track.id)
    analysis.beat_positions = [0.5, 1.0]
    analysis.waveform_peaks = [0.2, 0.8]
    session.add(analysis)
    session.commit()

    body = client.get("/api/metadata", params={"track_id": track.id}).json()
    assert "beat_positions" not in body
    assert body["analyzed"] is True
    assert len(body["waveform_peaks"]) == 2
