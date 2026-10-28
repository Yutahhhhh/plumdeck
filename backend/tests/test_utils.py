import pytest
import json
import os
from sqlmodel import Session
from utils import audio_math, filesystem, metadata, logger
from models import Track

def test_audio_math_normalize_key():
    assert audio_math.normalize_key("C Major") == "8B"
    assert audio_math.normalize_key("8A") == "8A"
    assert audio_math.normalize_key("Unknown Key") is None
    assert audio_math.normalize_key("") is None

def test_calculate_mixability_score():
    score = audio_math.calculate_mixability_score(120, "8B", 120, "8B", 1.0)
    assert score == 1.0
    # Key matching is optional; legacy recommendations no longer rank by key.
    score_adj = audio_math.calculate_mixability_score(120, "8B", 120, "8A", 1.0)
    assert score_adj == score
    # No key
    score_none = audio_math.calculate_mixability_score(120, None, 120, None)
    assert score_none > 0

def test_filesystem_resolve_path(tmp_path):
    f = tmp_path / "テスト.mp3"
    f.touch()
    assert filesystem.resolve_path(str(f)) is not None
    assert filesystem.resolve_path("/non/existent") is None

def test_metadata_smart_fallback(tmp_path, mocker):
    path = str(tmp_path / "song.mp3")
    
    # Mock TinyTag to return None for attributes so fallback logic triggers
    mock_tag = mocker.Mock()
    mock_tag.title = None
    mock_tag.artist = None
    mock_tag.album = None
    mock_tag.genre = None
    mocker.patch("tinytag.TinyTag.get", return_value=mock_tag)
    
    meta = metadata.extract_metadata_smart(path)
    assert meta["title"] == "song"
    assert meta["artist"] == "Unknown"


def test_metadata_smart_uses_artist_title_filename(tmp_path, mocker):
    path = str(tmp_path / "Artist - Song.mp3")
    mock_tag = mocker.Mock(title=None, artist=None, album=None, genre=None, year=None)
    mocker.patch("tinytag.TinyTag.get", return_value=mock_tag)

    meta = metadata.extract_metadata_smart(path)
    assert meta["title"] == "Song"
    assert meta["artist"] == "Artist"

def test_update_file_metadata_unsupported(tmp_path):
    f = tmp_path / "test.txt"
    f.touch()
    assert metadata.update_file_metadata(str(f), lyrics="test") is False

def test_logger_setup():
    log = logger.get_logger("test_logger")
    assert log is not None
    log.info("Test Log")

def test_update_file_metadata_mp3_lyrics(tmp_path, mocker):
    # Mock mutagen to avoid needing real audio files
    mock_id3 = mocker.Mock()
    mock_uslt = mocker.Mock()
    
    # Mock ID3 constructor
    mocker.patch("utils.metadata.ID3", return_value=mock_id3)
    # Mock USLT constructor
    mock_uslt_class = mocker.patch("utils.metadata.USLT", return_value=mock_uslt)
    
    path = str(tmp_path / "test.mp3")
    # Create dummy file so os.path.exists/splitext works
    with open(path, "w") as f:
        f.write("dummy mp3 content")
        
    metadata.update_file_metadata(path, lyrics="Test Lyrics")
    
    # Verify USLT was initialized with empty desc
    # This confirms the fix for "descSeptember" bug where desc was hardcoded
    mock_uslt_class.assert_called_with(encoding=3, lang='eng', desc='', text="Test Lyrics")
    
    # Verify save was called
    mock_id3.save.assert_called_once()

def test_metadata_extraction_with_year(tmp_path, mocker):
    path = str(tmp_path / "year_song.mp3")
    
    # Mock TinyTag
    mock_tag = mocker.Mock()
    mock_tag.title = "Year Song"
    mock_tag.artist = "Artist"
    mock_tag.album = "Album"
    mock_tag.genre = "Genre"
    mock_tag.year = "1999" # String format often returned by libraries
    
    mocker.patch("tinytag.TinyTag.get", return_value=mock_tag)
    
    meta = metadata.extract_metadata_smart(path)
    assert meta["year"] == 1999
    
    # Test invalid year
    mock_tag.year = "invalid"
    meta = metadata.extract_metadata_smart(path)
    assert meta["year"] is None
