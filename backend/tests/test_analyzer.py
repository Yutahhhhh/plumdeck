import pytest
from unittest.mock import MagicMock
import numpy as np
from domain.services.analysis import constants
from domain.services.analysis.analyzer import AudioAnalyzer

# Mock constants to avoid import errors if constants.py depends on something
class MockConstants:
    RHYTHM_METHOD = 'degara'
    KEY_PROFILE_TYPE = 'edma'
    SAMPLE_RATE = 44100
    NORM_ENERGY = (0, 1)
    NORM_DANCEABILITY = (0, 1)
    NORM_BRIGHTNESS = (0, 1)
    NORM_NOISINESS = (0, 1)
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
        "energy": 0.5,
        "danceability": 0.6,
        "brightness": 0.4,
        "noisiness": 0.1,
    }

    result = mock_analyzer._format_result("/path/to/song.mp3", mock_tag, features)

    assert result["lyrics"] == "La la la"
    assert result["title"] == "Test Title"
    assert (result["key"], result["scale"]) == ("C major", "major")
    # Outputs without a reader are neither computed nor stored.
    for removed in ("loudness", "loudness_range", "spectral_flux", "spectral_rolloff", "contrast"):
        assert removed not in result
    assert "key_strength" not in result["features_extra"]


def test_selected_timbre_and_key_return_only_read_outputs(mock_analyzer, mocker):
    mocker.patch.object(mock_analyzer, "_load_audio", return_value=np.ones(44100, dtype=np.float32))
    mocker.patch.object(mock_analyzer, "_extract_timbre_features", return_value={
        "energy": 0.5, "danceability": 0.25, "brightness": 0.75, "noisiness": 0.1})
    mock_analyzer.key_extractor = MagicMock(return_value=("F", "minor", 0.9))
    result = mock_analyzer.analyze_selected("/song.wav", ["key", "timbre"])
    assert {k: v for k, v in result.items() if k != "features_extra"} == {
        "key": "F minor", "scale": "minor",
        "energy": 0.5, "danceability": 0.25, "brightness": 0.75, "noisiness": 0.1,
    }
    assert set(result["features_extra"]) == {"analysis_components"}

def test_analyze_leaves_out_components_rekordbox_provides(mock_analyzer, mocker):
    tag = MagicMock(title="Song", artist="Artist", album="Album", genre="House", year="2020", duration=180.0, extra={})
    mocker.patch.object(mock_analyzer, "_extract_metadata", return_value=tag)
    mocker.patch.object(mock_analyzer, "_load_audio", return_value=np.ones(44100, dtype=np.float32))
    mocker.patch.object(mock_analyzer, "_extract_timbre_features", return_value={
        "energy": 0.5, "danceability": 0.25, "brightness": 0.75, "noisiness": 0.1})
    mocker.patch("domain.services.analysis.analyzer.align_beats", side_effect=lambda audio, ticks, rate: ticks)
    mock_analyzer.rhythm_extractor = MagicMock(return_value=(120.0, np.array([0.5, 1.0, 1.5]), 3.0, None, None))
    mock_analyzer.key_extractor = MagicMock(return_value=("F", "minor", 0.9))
    mock_analyzer.embedding_algo = None

    result = mock_analyzer.analyze("/song.wav", skip_components=("rhythm", "key"))
    mock_analyzer.rhythm_extractor.assert_not_called()
    mock_analyzer.key_extractor.assert_not_called()
    assert (result["bpm"], result["key"], result["scale"]) == (0.0, "", "")
    extra = result["features_extra"]
    assert extra["analysis_components"] == {"timbre": constants.COMPONENT_VERSIONS["timbre"]}
    assert not {"beat_positions", "playback_grid", "bpm_confidence"} & extra.keys()
    assert len(extra["waveform_peaks"]) == 500 and result["analysis_level"] == "full"

    full = mock_analyzer.analyze("/song.wav")
    assert (full["bpm"], full["key"]) == (120.0, "F minor")
    assert set(full["features_extra"]["analysis_components"]) == {"rhythm", "key", "timbre"}
    assert full["features_extra"]["beat_positions"] == [0.5, 1.0, 1.5]


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
        "energy": 0.0,
        "danceability": 0.0,
        "brightness": 0.0,
        "noisiness": 0.0,
    }

    result = mock_analyzer._format_result("/path/to/song.mp3", mock_tag, features)

    assert result["lyrics"] is None
