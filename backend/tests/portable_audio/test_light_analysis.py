import numpy as np
import pytest
import builtins
import shutil
import subprocess
import sys
import threading
import wave

from domain.services.analysis.portable import PortableAudioAnalyzer
from domain.services.analysis.light_dsp import SAMPLE_RATE, rhythm_and_key
from domain.services.analysis.light_grid import analyze as analyze_grid
from domain.services.analysis.beat_grid import playback_grid


def fixture_audio(bpm=126, root=0, minor=False, seconds=30, sample_rate=SAMPLE_RATE):
    t = np.arange(sample_rate * seconds) / sample_rate
    audio = np.zeros(len(t))
    for amplitude, interval in zip((.15, .12, .1), (0, 3 if minor else 4, 7)):
        frequency = 440 * 2 ** ((60 + root + interval - 69) / 12)
        audio += amplitude * np.sin(2 * np.pi * frequency * t)
    for beat in np.arange(.25, seconds, 60 / bpm):
        start = int(beat * sample_rate)
        count = min(int(sample_rate * .009), len(audio) - start)
        audio[start:start + count] += .5 * np.exp(-np.arange(count) / (sample_rate * .0016))
    return audio.astype(np.float32)


@pytest.mark.parametrize("bpm", [70, 80, 90, 100, 120, 126, 140, 174, 200, 220])
def test_light_estimates_known_tempos_without_librosa(bpm):
    actual, confidence, key, scale, _ = rhythm_and_key(fixture_audio(bpm=bpm))
    assert abs(actual - bpm) < 1
    assert confidence > .5
    assert (key, scale) == ("C", "major")


@pytest.mark.parametrize("root", range(12))
@pytest.mark.parametrize("minor", [False, True])
def test_light_estimates_transposed_major_and_minor_chords(root, minor):
    _, _, key, scale, strength = rhythm_and_key(fixture_audio(root=root, minor=minor, seconds=5))
    assert key == ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"][root]
    assert scale == ("minor" if minor else "major")
    assert strength > .5


def test_light_rejects_invalid_input_and_does_not_invent_silent_bpm():
    assert rhythm_and_key(np.zeros(SAMPLE_RATE))[0] == 0
    for audio in (np.array([]), np.array([np.nan]), np.ones(SAMPLE_RATE * 31)):
        with pytest.raises(ValueError):
            rhythm_and_key(audio)


@pytest.mark.parametrize("extension", ["wav", "mp3", "flac"])
def test_real_light_dsp_and_codecs_without_heavy_runtime(tmp_path, monkeypatch, extension):
    converter = shutil.which("ffmpeg")
    if not converter:
        pytest.skip("FFmpeg required for real decoder test")
    # A stereo 44.1 kHz file longer than the analysis window exercises seeking,
    # channel mixing and decoder-side downsampling. Keep Unicode/spaces in paths.
    source = tmp_path / "音楽 fixture.wav"
    mono = fixture_audio(seconds=40, sample_rate=44100)
    with wave.open(str(source), "wb") as output:
        output.setparams((2, 2, 44100, 0, "NONE", "not compressed"))
        output.writeframes((np.repeat(mono[:, None], 2, axis=1) * 32767).astype("<i2").tobytes())
    path = source
    if extension != "wav":
        path = source.with_suffix("." + extension)
        subprocess.run([converter, "-v", "error", "-i", str(source), str(path)], check=True, timeout=20)
    # Foreground playback must be ready without touching optional model work.
    def unexpected_embedding(*_args):
        raise AssertionError("Foreground light analysis must not compute an embedding")
    monkeypatch.setattr(PortableAudioAnalyzer, "_extract_light_embedding", unexpected_embedding)
    original_import = builtins.__import__

    def guarded(name, *args, **kwargs):
        if name.split(".")[0] in {"librosa", "numba", "scipy", "onnxruntime", "tensorflow"}:
            raise AssertionError(f"Light analysis imported {name}")
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", guarded)
    if extension == "mp3":
        import ingest
        from domain.services.analysis import portable
        monkeypatch.setattr(sys, "platform", "win32")
        monkeypatch.setattr(ingest, "_thread_local", threading.local())
        monkeypatch.setattr(portable, "find_ffmpeg", lambda: converter)
        result = ingest.analyze_track_file(str(path), analysis_profile="light")
    else:
        result = PortableAudioAnalyzer().analyze_light(str(path))
    assert abs(result["bpm"] - 126) < 1
    assert result["key"] == "C major"
    assert abs(result["duration"] - 40) < .2
    assert result["features_extra"]["analysis_window_seconds"] == 30
    assert result["features_extra"]["analysis_window_start_seconds"] == pytest.approx(5, abs=.1)
    assert result["analysis_level"] == "light"
    assert "embedding" not in result
    from utils.ingestion import has_completed_analysis_result
    assert has_completed_analysis_result(result)
    extra = result["features_extra"]
    assert extra["beat_positions"][0] < .3
    assert extra["beat_positions"][-1] > 39
    assert len(extra["waveform_peaks"]) == 500
    assert extra["playback_grid"]["bpm"] == pytest.approx(126, abs=.1)


def test_light_features_are_finite_bounded_approximations():
    analyzer = PortableAudioAnalyzer()
    samples = np.arange(44100 * 2, dtype=np.float32)
    envelope = .06 + .04 * np.sin(2 * np.pi * samples / 22050)
    audio = envelope * np.sin(2 * np.pi * 1500 * samples / 44100)

    result = analyzer._extract_light_features(audio.astype(np.float32))

    assert np.isfinite(list(result.values())).all()
    assert 0 < result["energy"] < 1
    assert 0 < result["brightness"] < 1
    assert 0 <= result["noisiness"] <= 1
    # Loudness, range, rolloff, flux and contrast had no reader and are not computed.
    assert set(result) == {"energy", "brightness", "noisiness"}


@pytest.mark.parametrize("bpm", [70, 90, 126, 174, 220])
def test_whole_track_grid_preserves_offset_and_does_not_accumulate_drift(bpm):
    estimated, ticks, _ = analyze_grid(fixture_audio(bpm=bpm, seconds=180))
    expected = np.arange(.25, 180, 60 / bpm)
    assert len(ticks) == len(expected)
    assert np.max(np.abs(ticks - expected)) < .015
    grid = playback_grid(ticks, estimated)
    assert grid["beat_times_ms"] is None
    assert abs(grid["bpm"] - bpm) < .02
    assert abs(grid["first_beat_ms"] - 250) < 15


@pytest.mark.parametrize("kind", ["change", "ramp", "silent_intro"])
def test_whole_track_grid_follows_tempo_changes_and_silent_intros(kind):
    seconds = 80
    times = np.arange(SAMPLE_RATE * seconds) / SAMPLE_RATE
    audio = .05 * np.sin(2 * np.pi * 261.626 * times)
    beat = 20.25 if kind == "silent_intro" else .25
    if kind == "silent_intro":
        audio[:20 * SAMPLE_RATE] = 0
    expected = []
    while beat < seconds:
        expected.append(beat)
        start = int(beat * SAMPLE_RATE)
        count = min(100, len(audio) - start)
        audio[start:start + count] += .6 * np.exp(-np.arange(count) / 17.5)
        bpm = (126 if beat < 40 else 93) if kind == "change" else (100 + beat * 30 / seconds if kind == "ramp" else 126)
        beat += 60 / bpm
    bpm, ticks, _ = analyze_grid(audio)
    errors = np.abs(ticks[:, None] - np.asarray(expected)[None, :])
    # Abrupt edits may need manual correction at the boundary. Do not flatten
    # their whole track to a constant grid or lose the rest of the beat series.
    assert np.mean(errors.min(axis=0) < .025) > .98
    assert np.percentile(errors.min(axis=1), 95) < .015
    grid = playback_grid(ticks, bpm)
    assert (grid["beat_times_ms"] is None) == (kind == "silent_intro")
    if kind == "silent_intro":
        assert 20_230 < grid["first_beat_ms"] < 20_270


def test_whole_track_grid_rejects_silence_and_invalid_audio():
    for audio in [np.zeros(SAMPLE_RATE * 8), np.ones(100), np.full(SAMPLE_RATE * 8, np.nan)]:
        with pytest.raises(ValueError):
            analyze_grid(audio)


def test_model_filter_bank_matches_detailed_preprocessing():
    librosa = pytest.importorskip("librosa")
    from domain.services.analysis.portable import musicnn_mel_filters
    np.testing.assert_array_equal(musicnn_mel_filters(), librosa.filters.mel(sr=16000, n_fft=512, n_mels=96, norm="slaney"))
