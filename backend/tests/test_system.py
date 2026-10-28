from sqlmodel import Session
from models import Track, TrackAnalysis

def test_health_check(client):
    """APIが生存しているか確認"""
    response = client.get("/api/")
    assert response.status_code == 200
    assert response.json()["status"] == "ok"
    assert "duckdb_version" in response.json()
    assert response.json()["ai_runtime"] == "mcp-client"

def test_dashboard_stats_empty(client):
    """初期状態でのダッシュボード統計を確認"""
    response = client.get("/api/dashboard")
    assert response.status_code == 200
    data = response.json()
    assert data["total_tracks"] == 0
    assert data["analyzed_tracks"] == 0
    assert len(data["genre_distribution"]) == 0

def test_dashboard_stats_with_data(client, session: Session):
    """データがある状態でのダッシュボード統計を確認"""
    # テストデータの投入
    track1 = Track(
        filepath="/Users/test/song1.mp3",
        title="Song 1",
        artist="Artist A",
        album="Album X",
        genre="Techno",
        bpm=128.0,
        duration=300.0, energy=.7, danceability=.6, brightness=.4, noisiness=.2,
        is_genre_verified=True
    )
    track2 = Track(
        filepath="/Users/test/song2.mp3",
        title="Song 2",
        artist="Artist B",
        album="Album Y",
        genre="House",
        bpm=0.0, # 未解析
        duration=240.0,
        is_genre_verified=False
    )
    session.add(track1)
    session.add(track2)
    session.flush()
    analysis = TrackAnalysis(track_id=track1.id)
    analysis.waveform_peaks = [0.2, 0.7]
    session.add(analysis)
    session.commit()

    response = client.get("/api/dashboard")
    data = response.json()
    
    assert data["total_tracks"] == 2
    assert data["analyzed_tracks"] == 1
    assert data["unanalyzed_tracks"] == 1
    assert data["unverified_genres_count"] == 1
    # ジャンル集計の確認
    genres = {g["name"]: g["count"] for g in data["genre_distribution"]}
    assert genres["Techno"] == 1

def test_save_file(client, tmp_path):
    file_path = tmp_path / "saved.txt"
    response = client.post("/api/system/save-file", json={
        "path": str(file_path),
        "content": "Hello World"
    })
    assert response.status_code == 200
    assert file_path.read_text() == "Hello World"

def test_reveal_file(client, mocker, tmp_path):
    # ファイル作成
    f = tmp_path / "reveal.txt"
    f.touch()
    
    # subprocess.run をモック
    mock_run = mocker.patch("subprocess.run")
    
    response = client.post("/api/system/reveal-file", json={"path": str(f)})
    assert response.status_code == 200
    assert response.json()["status"] == "success"
    
    mock_run.assert_called_once()
