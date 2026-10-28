"""Cold-worker light-analysis gate for the actual Windows distribution."""
import json
from pathlib import Path
import sys
import tempfile
import time
import wave


def analyze_file(path):
    started = time.monotonic()
    if sys.platform == "win32":
        # Exercise the same factory/profile dispatch as Explorer and Play.
        from ingest import analyze_track_file
        result = analyze_track_file(path, analysis_profile="light")
    else:
        from domain.services.analysis.portable import PortableAudioAnalyzer
        result = PortableAudioAnalyzer().analyze_light(path)
    forbidden = {"librosa", "numba", "tensorflow"}.intersection(sys.modules)
    assert not forbidden, f"Light worker loaded heavy dependencies: {forbidden}"
    assert result is not None
    assert abs(result["bpm"] - 126) < 1, result["bpm"]
    assert result["key"] == "C major", result["key"]
    assert result["analysis_level"] == "light"
    import numpy as np
    assert "embedding" not in result
    from utils.ingestion import has_completed_analysis_result
    assert has_completed_analysis_result(result)
    ticks = np.asarray(result["features_extra"]["beat_positions"])
    assert ticks[0] < .3 and ticks[-1] > 299
    assert np.max(np.abs(np.diff(ticks) - 60 / 126)) < .015
    assert len(result["features_extra"]["waveform_peaks"]) == 500
    from api.schemas.performance_metadata import BeatGrid
    grid = BeatGrid.model_validate(result["features_extra"]["playback_grid"])
    assert grid.first_beat_ms < 300 and abs(grid.bpm - 126) < .1
    assert result["features_extra"]["analysis_window_seconds"] == 30
    assert result["features_extra"]["analysis_sample_rate"] == 11025
    memory = {}
    if sys.platform != "win32":
        import resource
        memory["peak_rss_mb"] = round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / (1024 * 1024 if sys.platform == "darwin" else 1024), 1)
    return {"bpm": result["bpm"], "key": result["key"], "beats": len(ticks), "embedding_pending": True, **memory,
            "worker_seconds": round(time.monotonic() - started, 3)}


def run():
    import numpy as np
    from domain.services.analysis.process_runner import AnalysisExecutor
    sr = 11025
    t = np.arange(sr * 300) / sr
    audio = sum(amplitude * np.sin(2 * np.pi * frequency * t)
                for amplitude, frequency in ((.15, 261.626), (.12, 329.628), (.10, 391.995)))
    for beat in np.arange(.25, 300, 60 / 126):
        start = int(beat * sr)
        count = min(100, len(audio) - start)
        audio[start:start + count] += .5 * np.exp(-np.arange(count) / 17.5)
    results = []
    with tempfile.TemporaryDirectory(prefix="Plumdeck 音楽 ") as directory:
        path = Path(directory) / "light fixture.wav"
        with wave.open(str(path), "wb") as output:
            output.setparams((1, 2, sr, 0, "NONE", "not compressed"))
            output.writeframes((np.clip(audio, -1, 1) * 32767).astype("<i2").tobytes())
        # A fresh process for each track, with a budget including cold imports,
        # FFmpeg and DSP. Run before the full diagnostic to prevent warm JIT
        # caches from masking an accidental heavy-runtime dependency. Submit via
        # AnalysisExecutor so the packaged gate covers the same server-thread ->
        # worker boundary used by the UI, rather than calling run_isolated on the
        # diagnostic's main thread.
        with AnalysisExecutor(max_workers=1, task_timeout=30) as executor:
            for _ in range(2):
                stages = []
                started = time.monotonic()
                future = executor.submit_with_progress(
                    analyze_file, str(path), on_progress=lambda event: stages.append(event["stage"])
                )
                result = future.result(timeout=35)
                assert "metadata" in stages, f"Worker never reached audio analysis: {stages}"
                result["progress_stages"] = stages
                result["total_seconds"] = round(time.monotonic() - started, 3)
                results.append(result)
    print(json.dumps({"ok": True, "profile": "light", "cold_workers": results}))


if __name__ == "__main__":
    import multiprocessing
    multiprocessing.freeze_support()
    run()
