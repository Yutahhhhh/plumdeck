import pytest
import os
import asyncio
from unittest.mock import MagicMock, AsyncMock
from domain.services.ingestion_domain_service import IngestionDomainService

@pytest.mark.asyncio
async def test_process_track_ingestion_imports_lrc(tmp_path, mocker):
    # Setup files
    mp3_path = tmp_path / "test_track.mp3"
    lrc_path = tmp_path / "test_track.lrc"
    
    mp3_path.touch()
    lrc_path.write_text("LRC Lyrics Content", encoding="utf-8")
    
    # Mock dependencies
    mocker.patch("domain.services.ingestion_domain_service.Session")
    mocker.patch("domain.services.ingestion_domain_service.db_connection")
    mocker.patch("domain.services.ingestion_domain_service.select")
    
    # Mock update_file_metadata to verify it gets called with LRC content
    mock_update_metadata = mocker.patch("domain.services.ingestion_domain_service.update_file_metadata")
    
    # Mock other calls to avoid errors
    mocker.patch("domain.services.ingestion_domain_service.TinyTag.get")
    mocker.patch("domain.services.ingestion_domain_service.extract_metadata_smart", return_value={
        "title": "Title", "artist": "Artist", "album": "Album", "genre": "Genre"
    })
    mocker.patch("domain.services.ingestion_domain_service.analyze_track_file", return_value={
        "filepath": str(mp3_path), "title": "Title", "artist": "Artist",
        "duration": 60, "bpm": 120, "energy": .7, "danceability": .6, "brightness": .4, "noisiness": .2,
        "features_extra": {"waveform_peaks": [.2, .7]},
    })
    mocker.patch("domain.services.ingestion_domain_service.has_valid_metadata", return_value=True)
    
    service = IngestionDomainService()
    loop = asyncio.get_running_loop()
    
    # Run ingestion
    await service.process_track_ingestion(
        filepath=str(mp3_path),
        force_update=False,
        loop=loop,
        save_to_db=False
    )
    
    # Verify update_file_metadata was called with the lyrics from the file
    mock_update_metadata.assert_called_once_with(str(mp3_path), "LRC Lyrics Content")

@pytest.mark.asyncio
async def test_process_track_ingestion_lyrics_priority(tmp_path, mocker):
    # Setup files
    mp3_path = tmp_path / "priority_test.mp3"
    lrc_path = tmp_path / "priority_test.lrc"
    
    mp3_path.touch()
    lrc_path.write_text("LRC Lyrics", encoding="utf-8")
    
    # Mock dependencies
    mocker.patch("domain.services.ingestion_domain_service.Session")
    mocker.patch("domain.services.ingestion_domain_service.db_connection")
    mocker.patch("domain.services.ingestion_domain_service.select")
    mocker.patch("domain.services.ingestion_domain_service.update_file_metadata")
    mocker.patch("domain.services.ingestion_domain_service.TinyTag.get")
    mocker.patch("domain.services.ingestion_domain_service.extract_metadata_smart", return_value={
        "title": "Title", "artist": "Artist", "album": "Album", "genre": "Genre"
    })
    mocker.patch("domain.services.ingestion_domain_service.has_valid_metadata", return_value=True)
    
    # Mock analyze_track_file to return embedded lyrics
    mocker.patch("domain.services.ingestion_domain_service.analyze_track_file", return_value={
        "lyrics": "Embedded Lyrics",
        "title": "Title",
        "artist": "Artist",
        "filepath": str(mp3_path),
        "duration": 60,
        "bpm": 120,
        "energy": .7, "danceability": .6, "brightness": .4, "noisiness": .2,
        "features_extra": {"waveform_peaks": [.2, .7]},
    })
    
    service = IngestionDomainService()
    loop = asyncio.get_running_loop()
    
    # Run ingestion
    result = await service.process_track_ingestion(
        filepath=str(mp3_path),
        force_update=False,
        loop=loop,
        save_to_db=False
    )
    
    # Should prioritize LRC lyrics over embedded lyrics
    assert result["lyrics"] == "LRC Lyrics"

@pytest.mark.asyncio
async def test_process_track_ingestion_embedded_lyrics_fallback(tmp_path, mocker):
    # Setup files - NO LRC file
    mp3_path = tmp_path / "embedded_test.mp3"
    mp3_path.touch()
    
    # Mock dependencies
    mocker.patch("domain.services.ingestion_domain_service.Session")
    mocker.patch("domain.services.ingestion_domain_service.db_connection")
    mocker.patch("domain.services.ingestion_domain_service.select")
    mocker.patch("domain.services.ingestion_domain_service.update_file_metadata")
    mocker.patch("domain.services.ingestion_domain_service.TinyTag.get")
    mocker.patch("domain.services.ingestion_domain_service.extract_metadata_smart", return_value={
        "title": "Title", "artist": "Artist", "album": "Album", "genre": "Genre"
    })
    mocker.patch("domain.services.ingestion_domain_service.has_valid_metadata", return_value=True)
    mocker.patch("domain.services.ingestion_domain_service.check_metadata_changed", return_value=False)
    
    # Directly mock analyze_track_file on the module where it's used
    mocker.patch("domain.services.ingestion_domain_service.analyze_track_file", return_value={
        "lyrics": "Embedded Lyrics",
        "title": "Title",
        "artist": "Artist",
        "bpm": 120,
        "duration": 60,
        "energy": .7, "danceability": .6, "brightness": .4, "noisiness": .2,
        "features_extra": {"waveform_peaks": [.2, .7]},
        "filepath": str(mp3_path)
    })
    
    service = IngestionDomainService()
    loop = asyncio.get_running_loop()
    
    # Run ingestion with force_update=True to bypass DB checks
    result = await service.process_track_ingestion(
        filepath=str(mp3_path),
        force_update=True,
        loop=loop,
        save_to_db=False
    )
    
    # Should use embedded lyrics since no LRC
    assert result is not None, "Result should not be None"
    assert result["lyrics"] == "Embedded Lyrics"
