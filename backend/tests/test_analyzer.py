import pytest
from unittest.mock import MagicMock
import numpy as np
from domain.services.analysis import constants
from domain.services.analysis.analyzer import AudioAnalyzer

# Mock constants to avoid import errors if constants.py depends on something
class MockConstants:
    RHYTHM_METHOD = 'degara'
    KEY_PROFILE_TYPE = 'edma'
    WINDOW_TYPE = 'hann'
    SAMPLE_RATE = 44100
    FRAME_SIZE = 2048
    HOP_SIZE = 1024
    NORM_ENERGY = (0, 1)
    NORM_DANCEABILITY = (0, 1)
    NORM_BRIGHTNESS = (0, 1)
    NORM_NOISINESS = (0, 1)
    NORM_FLUX = (0, 1)
    NORM_LOUDNESS_RANGE = (0, 1)
    COMPONENT_VERSIONS = constants.COMPONENT_VERSIONS

@pytest.fixture
def mock_analyzer(mocker):
    mocker.patch("domain.services.analysis.analyzer.constants", MockConstants)
    mocker.patch("domain.services.analysis.analyzer.HAS_ESSENTIA", True)
    
    # Mock Essentia algorithms
    mocker.patch("domain.services.analysis.analyzer.es.RhythmExtractor2013")
    mocker.patch("domain.services.analysis.analyzer.es.KeyExtractor")
    mocker.patch("domain.services.analysis.analyzer.es.RMS")
    mocker.patch("domain.services.analysis.analyzer.es.Danceability")
    mocker.patch("domain.services.analysis.analyzer.es.SpectralCentroidTime")
    mocker.patch("domain.services.analysis.analyzer.es.ZeroCrossingRate")
    mocker.patch("domain.services.analysis.analyzer.es.Spectrum")
    mocker.patch("domain.services.analysis.analyzer.es.Windowing")
    mocker.patch("domain.services.analysis.analyzer.es.RollOff")
    mocker.patch("domain.services.analysis.analyzer.es.Flux")
    mocker.patch("domain.services.analysis.analyzer.es.TensorflowPredictMusiCNN", side_effect=Exception("Model not found")) # Skip model loading

    analyzer = AudioAnalyzer()
    return analyzer

def test_format_result_with_lyrics(mock_analyzer, mocker):
    # Mock TinyTag
    mock_tag = MagicMock()
    mock_tag.title = "Test Title"
    mock_tag.artist = "Test Artist"
    mock_tag.album = "Test Album"
    mock_tag.genre = "Test Genre"
    mock_tag.year = "2023"
    mock_tag.duration = 180.0
    # Mock extra lyrics
    mock_tag.extra = {"lyrics": "La la la"}

    # Mock features
    features = {
        "bpm": 120.0,
        "beat_positions": np.array([1.0, 2.0]),
        "bpm_confidence": 1.0,
        "key": "C",
        "scale": "major",
        "key_strength": 1.0,
        "energy": 0.5,
        "danceability": 0.6,
        "loudness": -10.0,
        "loudness_range": 5.0,
        "brightness": 0.4,
        "noisiness": 0.1,
        "rolloff": 1000.0,
        "flux": 0.2
    }

    result = mock_analyzer._format_result("/path/to/song.mp3", mock_tag, features)

    assert result["lyrics"] == "La la la"
    assert result["title"] == "Test Title"

def test_format_result_without_lyrics(mock_analyzer, mocker):
    # Mock TinyTag
    mock_tag = MagicMock()
    mock_tag.title = "Test Title"
    mock_tag.extra = {} # No lyrics

    # Mock features
    features = {
        "bpm": 120.0,
        "beat_positions": np.array([]),
        "bpm_confidence": 0.0,
        "key": "C",
        "scale": "major",
        "key_strength": 0.0,
        "energy": 0.0,
        "danceability": 0.0,
        "loudness": 0.0,
        "loudness_range": 0.0,
        "brightness": 0.0,
        "noisiness": 0.0,
        "rolloff": 0.0,
        "flux": 0.0
    }

    result = mock_analyzer._format_result("/path/to/song.mp3", mock_tag, features)

    assert result["lyrics"] is None
