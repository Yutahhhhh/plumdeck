from sqlmodel import Session
from models import Track

def test_get_tracks_filtering(client, session: Session):
    """楽曲一覧のフィルタリング機能のE2Eテスト"""
    # 1. セットアップ
    tracks = [
        Track(filepath="/path/1.mp3", title="A-Track", artist="Art1", album="Album1", genre="House", bpm=120, duration=100),
        Track(filepath="/path/2.mp3", title="B-Track", artist="Art2", album="Album2", genre="Techno", bpm=130, duration=100),
        Track(filepath="/path/3.mp3", title="C-Track", artist="Art1", album="Album1", genre="House", bpm=122, duration=100),
    ]
    for t in tracks: session.add(t)
    session.commit()

    # 2. ジャンルフィルタ
    response = client.get("/api/tracks", params={"genres": ["House"]})
    assert len(response.json()) == 2
    
    # 3. アーティスト検索 (Partial match)
    response = client.get("/api/tracks", params={"artist": "Art2"})
    assert len(response.json()) == 1
    assert response.json()[0]["title"] == "B-Track"

    # 4. BPM範囲検索 (BPM 120 +/- 5)
    response = client.get("/api/tracks", params={"bpm": 120, "bpm_range": 5.0})
    # 120 と 122 がヒットするはず
    assert len(response.json()) == 2

    # 5. Subgenre Filter (using subgenres param)
    t4 = Track(filepath="/path/4.mp3", title="D-Track", artist="Art3", album="Album3", genre="House", subgenre="Deep House", bpm=120, duration=100)
    session.add(t4)
    session.commit()
    
    response = client.get("/api/tracks", params={"subgenres": ["Deep House"]})
    assert len(response.json()) == 1
    assert response.json()[0]["title"] == "D-Track"

def test_get_tracks_query_searches_title_or_artist(client, session: Session):
    tracks = [
        Track(filepath="/path/title.mp3", title="Midnight City", artist="M83", album="Album1", genre="Synth", bpm=105, duration=100),
        Track(filepath="/path/artist.mp3", title="Intro", artist="Midnight Runners", album="Album2", genre="Disco", bpm=118, duration=100),
        Track(filepath="/path/other.mp3", title="Sunrise", artist="Someone Else", album="Album3", genre="Synth", bpm=122, duration=100),
    ]
    for t in tracks:
        session.add(t)
    session.commit()

    response = client.get("/api/tracks", params={"q": "Midnight"})
    assert response.status_code == 200
    titles = {track["title"] for track in response.json()}
    assert titles == {"Midnight City", "Intro"}

    response = client.get("/api/tracks", params={"q": "Midnight", "genres": ["Synth"]})
    assert response.status_code == 200
    titles = {track["title"] for track in response.json()}
    assert titles == {"Midnight City"}

def test_get_tracks_query_searches_filepath_and_metadata_tokens(client, session: Session):
    track = Track(
        filepath="/music/DJ City/Eve - Who's That Girl_ - Max Wallin Remix (Clean).mp3",
        title="Who's That Girl? - Max Wallin Remix (Clean)",
        artist="Eve",
        album="Unknown",
        genre="Hip Hop",
        subgenre="Remix",
        bpm=104,
        duration=153,
    )
    session.add(track)
    session.commit()

    response = client.get(
        "/api/tracks",
        params={"q": "Eve - Who's That Girl_ - Max Wallin Remix (Clean).mp3"},
    )
    assert response.status_code == 200
    titles = {track["title"] for track in response.json()}
    assert "Who's That Girl? - Max Wallin Remix (Clean)" in titles

    response = client.get("/api/tracks", params={"q": "Hip Hop Remix"})
    assert response.status_code == 200
    titles = {track["title"] for track in response.json()}
    assert "Who's That Girl? - Max Wallin Remix (Clean)" in titles

def test_update_track_genre(client, session: Session):
    """ジャンル更新APIのテスト"""
    track = Track(filepath="/p/1.mp3", title="T", artist="A", album="AlbumT", genre="Old", bpm=120, duration=100)
    session.add(track)
    session.commit()
    session.refresh(track)

    response = client.patch(
        f"/api/tracks/{track.id}/genre",
        json={"genre": "New Genre"}
    )
    assert response.status_code == 200
    assert response.json()["genre"] == "New Genre"
    assert response.json()["is_genre_verified"] is True

    # DB側も更新されているか確認
    session.refresh(track)
    assert track.genre == "New Genre"

def test_structured_target_search(session: Session):
    """A connected MCP model can pass structured targets without an internal LLM."""
    from app.services.track_app_service import TrackAppService
    t1 = Track(filepath="/v1.mp3", title="Chill", energy=0.2, bpm=90, duration=100, artist="A", album="B", genre="C")
    t2 = Track(filepath="/v2.mp3", title="Energy", energy=0.9, bpm=140, duration=100, artist="A", album="B", genre="C")
    session.add(t1)
    session.add(t2)
    session.commit()

    data = TrackAppService(session).get_tracks(target_params={"bpm": 140, "energy": 0.9})
    assert data[0]["title"] == "Energy"

def test_suggest_genre(client, session: Session, mocker):
    t1 = Track(filepath="/s1.mp3", title="S1", artist="A", album="B", genre="Unknown", bpm=120, duration=100)
    session.add(t1)
    session.commit()
    
    # RecommendationAppService.suggest_genre をモック
    # 実際にはLLMやルールベースで提案するが、ここではモック
    mock_suggest = mocker.patch("app.services.recommendation_app_service.RecommendationAppService.suggest_genre")
    mock_suggest.return_value = {"suggested_genre": "Techno", "reason": "BPM"}
    
    response = client.get(f"/api/tracks/{t1.id}/suggest-genre")
    assert response.status_code == 200
    assert response.json()["suggested_genre"] == "Techno"

def test_get_similar_tracks(client, session: Session):
    t1 = Track(filepath="/sim1.mp3", title="Sim1", artist="A", album="B", genre="G", bpm=120, duration=100)
    t2 = Track(filepath="/sim2.mp3", title="Sim2", artist="A", album="B", genre="G", bpm=120, duration=100)
    session.add(t1)
    session.add(t2)
    session.commit()
    
    from models import TrackEmbedding
    import json
    vec = [0.1] * 200
    te1 = TrackEmbedding(track_id=t1.id, embedding_json=json.dumps(vec))
    te2 = TrackEmbedding(track_id=t2.id, embedding_json=json.dumps(vec))
    session.add(te1)
    session.add(te2)
    session.commit()
    
    response = client.get(f"/api/tracks/{t1.id}/similar")
    assert response.status_code == 200
    data = response.json()
    assert len(data) > 0
    assert data[0]["title"] == "Sim2"

def test_genre_search(client, session: Session):
    """ジャンル検索のテスト（genre/subgenre両方でマッチ）"""
    # 1. データ準備
    t1 = Track(filepath="/p.mp3", title="House Track", artist="A", album="B", genre="House", subgenre="Deep House", bpm=120, duration=100)
    t2 = Track(filepath="/c.mp3", title="Techno Track", artist="A", album="B", genre="Techno", subgenre="Minimal", bpm=120, duration=100)
    t3 = Track(filepath="/o.mp3", title="Rock Track", artist="A", album="B", genre="Rock", bpm=120, duration=100)
    
    session.add(t1)
    session.add(t2)
    session.add(t3)
    session.commit()

    # 2. ジャンル検索 (House)
    response = client.get("/api/tracks", params={"genres": ["House"]})
    assert response.status_code == 200
    data = response.json()
    titles = [t["title"] for t in data]
    assert "House Track" in titles
    assert "Techno Track" not in titles
    assert "Rock Track" not in titles



def test_get_tracks_year_filtering(client, session: Session):
    """リリース年によるフィルタリング機能のテスト"""
    # 1. セットアップ
    tracks = [
        Track(filepath="/y1.mp3", title="90s Track", artist="A", album="B", genre="G", bpm=120, duration=100, year=1995),
        Track(filepath="/y2.mp3", title="2000s Track", artist="A", album="B", genre="G", bpm=120, duration=100, year=2005),
        Track(filepath="/y3.mp3", title="Recent Track", artist="A", album="B", genre="G", bpm=120, duration=100, year=2023),
        Track(filepath="/y4.mp3", title="No Year Track", artist="A", album="B", genre="G", bpm=120, duration=100, year=None),
    ]
    for t in tracks: session.add(t)
    session.commit()

    # 2. 範囲指定 (2000 - 2010) -> 2005年の曲のみヒット
    response = client.get("/api/tracks", params={"min_year": 2000, "max_year": 2010})
    data = response.json()
    assert len(data) == 1
    assert data[0]["title"] == "2000s Track"

    # 3. 最小年指定 (2020以降) -> 2023年の曲のみヒット
    response = client.get("/api/tracks", params={"min_year": 2020})
    data = response.json()
    assert len(data) == 1
    assert data[0]["title"] == "Recent Track"

    # 4. 最大年指定 (1999以前) -> 1995年の曲のみヒット
    response = client.get("/api/tracks", params={"max_year": 1999})
    data = response.json()
    assert len(data) == 1
    assert data[0]["title"] == "90s Track"

def test_get_tracks_includes_year(client, session: Session):
    """APIレスポンスにyearが含まれているか確認"""
    track = Track(
        filepath="/path/year.mp3", 
        title="Year Track", 
        artist="Artist", 
        album="Album", 
        genre="Pop", 
        year=1999, 
        bpm=120, 
        duration=100
    )
    session.add(track)
    session.commit()

    response = client.get("/api/tracks")
    data = response.json()
    
    assert len(data) == 1
    assert data[0]["year"] == 1999


def test_get_tracks_includes_analysis_level(client, session: Session):
    """The library can distinguish light analysis from full and legacy rows."""
    tracks = [
        Track(
            filepath="/path/light-analysis.mp3",
            title="Light Analysis Track",
            artist="Artist",
            album="Album",
            genre="House",
            bpm=120,
            duration=100,
            analysis_level="light",
        ),
        Track(
            filepath="/path/legacy-analysis.mp3",
            title="Legacy Analysis Track",
            artist="Artist",
            album="Album",
            genre="House",
            bpm=120,
            duration=100,
            analysis_level=None,
        ),
    ]
    session.add_all(tracks)
    session.commit()

    response = client.get("/api/tracks")

    assert response.status_code == 200
    levels_by_title = {
        track["title"]: track["analysis_level"] for track in response.json()
    }
    assert levels_by_title == {
        "Light Analysis Track": "light",
        "Legacy Analysis Track": None,
    }


def test_key_filter_ignores_case_and_enharmonic_spelling(client, session: Session):
    keys = {"/k/1.mp3": "F minor", "/k/2.mp3": "Eb minor", "/k/3.mp3": "C major", "/k/4.mp3": "2A"}
    for path, key in keys.items():
        session.add(Track(filepath=path, title=path, artist="A", genre="House", bpm=120, duration=100, key=key))
    session.commit()

    def paths(key):
        return sorted(t["filepath"] for t in client.get("/api/tracks", params={"key": key}).json())

    # The UI sends 'Minor' / 'F Minor' / 'D# Minor'; analysis stores 'F minor' / 'Eb minor'.
    assert paths("Minor") == ["/k/1.mp3", "/k/2.mp3"]
    assert paths("Major") == ["/k/3.mp3"]
    assert paths("F Minor") == ["/k/1.mp3"]
    assert paths("D# Minor") == ["/k/2.mp3", "/k/4.mp3"]
