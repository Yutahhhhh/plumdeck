"""Native Windows analysis: packaged FFmpeg, librosa and MusiCNN via ONNX Runtime.

No WSL, Python installation or model download is required on the user's PC.
"""
from pathlib import Path
import subprocess
import sys
import tempfile

import numpy as np

from .analyzer import AudioAnalyzer
from . import constants
from . import light_dsp, light_grid
from .beat_grid import playback_grid, VERSION as GRID_VERSION
from .progress import report
from utils.executables import find_ffmpeg


LIGHT_ANALYSIS_SECONDS = light_dsp.WINDOW_SECONDS


def musicnn_mel_filters():
    """Slaney's piecewise mel scale, identical to the full-path model input.

    Kept in NumPy so a 3-second inference does not import librosa/Numba or
    trigger JIT compilation. Float32 weights match librosa.filters.mel.
    """
    def to_mel(hz):
        return np.where(hz >= 1000, 15 + np.log(np.maximum(hz, 1) / 1000) / (np.log(6.4) / 27), hz / (200 / 3))
    mels = np.linspace(0, to_mel(np.array(8000.)), 98)
    hz = np.where(mels >= 15, 1000 * np.exp((mels - 15) * (np.log(6.4) / 27)), (200 / 3) * mels)
    ramps = hz[:, None] - np.fft.rfftfreq(512, 1 / 16000)[None, :]
    widths = np.diff(hz)
    weights = np.maximum(0, np.minimum(-ramps[:-2] / widths[:-1, None], ramps[2:] / widths[1:, None])).astype(np.float32)
    weights *= (2 / (hz[2:] - hz[:-2]))[:, None]
    return weights


def musicnn_bands(audio: np.ndarray) -> np.ndarray:
    # Essentia TensorflowInputMusiCNN: centered 512-sample symmetric Hann,
    # 256 hop, Slaney area-normalized 96 mel power bands, log10(1+10000*x).
    padded = np.pad(np.asarray(audio, dtype=np.float32), (256, 256 + (-len(audio) % 256)))
    frames = np.lib.stride_tricks.sliding_window_view(padded, 512)[::256]
    spectrum = np.abs(np.fft.rfft(frames * np.hanning(512), axis=1)) ** 2
    mel = musicnn_mel_filters()
    return np.log10(1 + 10000 * (spectrum @ mel.T)).astype(np.float32)


class PortableAudioAnalyzer(AudioAnalyzer):
    def __init__(self):
        self.rhythm_extractor = self._rhythm
        self.key_extractor = self._key
        self.embedding_algo = True  # The model is loaded only when embedding is requested.
        self._embedding_session = None

    @property
    def librosa(self):
        # Light analysis must never import librosa/Numba, including during
        # construction in a fresh spawned or packaged worker.
        import librosa
        return librosa

    def _load_audio(self, filepath, max_seconds=1800):
        converter = find_ffmpeg()
        if not converter:
            raise RuntimeError("同梱の音声デコーダーが見つかりません")
        # Spool decoded PCM to disk instead of keeping stdout plus a second
        # NumPy copy in memory. Bound decoding before allocating the array.
        with tempfile.TemporaryFile() as pcm:
            decoded = subprocess.run(
                [converter, "-nostdin", "-v", "error", "-threads", "1", "-i", str(filepath),
                 "-t", str(max_seconds + 1), "-vn", "-ac", "1", "-ar", str(constants.SAMPLE_RATE),
                 "-f", "f32le", "pipe:1"],
                stdout=pcm, stderr=subprocess.PIPE, timeout=300, check=False,
                creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            if pcm.tell() > max_seconds * constants.SAMPLE_RATE * 4:
                raise ValueError("Audio analysis supports tracks up to 30 minutes")
            pcm.seek(0)
            audio = np.fromfile(pcm, dtype="<f4")
        if decoded.returncode:
            raise RuntimeError("音声をデコードできません: " + decoded.stderr.decode("utf-8", errors="replace")[-2000:])
        if not audio.size or not np.isfinite(audio).all():
            raise ValueError("Audio is empty or contains non-finite samples")
        return audio

    def _load_audio_segment(self, filepath, start_seconds=0.0, duration_seconds=LIGHT_ANALYSIS_SECONDS, sample_rate=light_dsp.SAMPLE_RATE):
        """Decode bounded mono PCM for whole-track DSP or small model windows.

        Placing ``-ss`` before the input lets FFmpeg seek without decoding the
        preceding audio.  This is deliberately separate from ``_load_audio``:
        detailed analysis keeps its existing whole-track behaviour.
        """
        converter = find_ffmpeg()
        if not converter:
            raise RuntimeError("同梱の音声デコーダーが見つかりません")
        command = [converter, "-nostdin", "-v", "error", "-threads", "1"]
        if start_seconds > 0:
            command.extend(["-ss", f"{start_seconds:.3f}"])
        command.extend([
            "-i", str(filepath), "-t", str(duration_seconds), "-vn", "-ac", "1",
            "-ar", str(sample_rate), "-f", "f32le", "pipe:1",
        ])
        # Whole-track low-resolution PCM and small model excerpts share a
        # bounded decoder without holding both stdout and an array in RAM.
        with tempfile.TemporaryFile() as pcm:
            decoded = subprocess.run(
                command, stdout=pcm, stderr=subprocess.PIPE, timeout=30,
                check=False, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            )
            if decoded.returncode:
                raise RuntimeError("音声をデコードできません: " + decoded.stderr.decode("utf-8", errors="replace")[-2000:])
            pcm.seek(0)
            audio = np.fromfile(pcm, dtype="<f4", count=int(duration_seconds * sample_rate))
        if not audio.size or not np.isfinite(audio).all():
            raise ValueError("Audio is empty or contains non-finite samples")
        return audio

    def _load_grid_audio(self, filepath):
        audio = self._load_audio_segment(filepath, 0, light_grid.MAX_SECONDS + 1)
        if len(audio) > light_grid.MAX_SECONDS * light_dsp.SAMPLE_RATE:
            raise ValueError("Audio analysis supports tracks up to 30 minutes")
        return audio

    def _extract_light_features(self, audio, sample_rate=constants.SAMPLE_RATE):
        """Return inexpensive, finite descriptors from a bounded audio window."""
        rms = float(np.sqrt(np.mean(np.square(audio, dtype=np.float64))))
        if len(audio) > 1:
            noisiness = float(np.mean(np.signbit(audio[1:]) != np.signbit(audio[:-1])))
        else:
            noisiness = 0.0

        # A short FFT slice is enough for an approximate brightness indicator
        # and avoids constructing a full-track STFT matrix.
        fft_samples = min(len(audio), sample_rate * 10)
        sample = audio[(len(audio) - fft_samples) // 2:][:fft_samples]
        frame_size, hop = 2048, 1024
        if len(sample) < frame_size:
            sample = np.pad(sample, (0, frame_size - len(sample)))
        frames = np.lib.stride_tricks.sliding_window_view(sample, frame_size)[::hop]
        spectra = np.abs(np.fft.rfft(frames * np.hanning(frame_size), axis=1))
        spectrum = np.mean(spectra, axis=0)
        frequencies = np.fft.rfftfreq(frame_size, 1 / sample_rate)
        total = float(np.sum(spectrum))
        brightness = float(np.dot(spectrum, frequencies) / total) if total > 0 else 0.0
        return {
            "energy": float(np.clip(rms / constants.NORM_ENERGY[1], 0, 1)),
            "brightness": float(np.clip(
                (brightness - constants.NORM_BRIGHTNESS[0])
                / (constants.NORM_BRIGHTNESS[1] - constants.NORM_BRIGHTNESS[0]), 0, 1,
            )),
            "noisiness": float(np.clip(noisiness / constants.NORM_NOISINESS[1], 0, 1)),
        }

    def analyze_light(self, filepath, external_lyrics=None):
        """Feature-complete Windows analysis with bounded DSP and model work."""
        report("metadata", "曲名・長さなどの音源情報を読み取っています")
        tag = self._extract_metadata(filepath)
        report("decode", "全曲の音源を低解像度で読み込んでいます（11.025kHz）")
        whole_audio = self._load_grid_audio(filepath)
        duration = len(whole_audio) / light_dsp.SAMPLE_RATE
        start_sample = max(0, (len(whole_audio) - light_dsp.SAMPLE_RATE * LIGHT_ANALYSIS_SECONDS) // 2)
        start = start_sample / light_dsp.SAMPLE_RATE
        audio = whole_audio[start_sample:start_sample + light_dsp.SAMPLE_RATE * LIGHT_ANALYSIS_SECONDS]
        report("rhythm_key", "代表区間のBPM・キーを解析しています（最大30秒）")
        bpm_hint, _, key, scale, _ = light_dsp.rhythm_and_key(audio)
        report("beat_grid", "全曲のビート位置を解析しています")
        bpm, ticks, confidence = light_grid.analyze(whole_audio, bpm_hint=bpm_hint)
        grid = playback_grid(ticks, bpm, confidence)
        if grid is None:
            raise ValueError("軽量解析でビートグリッドを取得できませんでした")

        report("waveform", "全曲の概要波形を作成しています")
        extra = {
            "analysis_level": "light",
            "analysis_profile": "light",
            "analysis_window_start_seconds": round(start, 3),
            "analysis_window_seconds": round(len(audio) / light_dsp.SAMPLE_RATE, 3),
            "analysis_sample_rate": light_dsp.SAMPLE_RATE,
            "analysis_components": {
                "rhythm": "numpy-attacks-v2",
                "key": "numpy-chroma-light-v2",
                "timbre": "bounded-summary-v2",
            },
            "bpm_confidence": round(float(confidence), 2),
            "beat_positions": ticks.tolist(),
            "playback_grid_version": GRID_VERSION,
            "playback_grid": grid,
            "waveform_peaks": light_grid.waveform_peaks(whole_audio),
        }
        metadata = tag
        report("timbre", "音量・明るさなどの基本情報を計算しています")
        result = {
            "filepath": str(filepath),
            "title": ((getattr(metadata, "title", None) or "").strip()
                      or Path(filepath).stem),
            "artist": (getattr(metadata, "artist", None) or "Unknown").strip(),
            "album": (getattr(metadata, "album", None) or "Unknown").strip(),
            "genre": (getattr(metadata, "genre", None) or "Unknown").strip(),
            "year": None,
            "lyrics": external_lyrics or (
                metadata.extra.get("lyrics") if metadata and hasattr(metadata, "extra") else None
            ),
            "duration": duration,
            "bpm": round(float(bpm), 2),
            "key": f"{key} {scale}",
            "scale": scale,
            # A bounded beat-stability estimate is safer than storing an
            # unknown value as the genuine minimum of the feature.
            "danceability": float(np.clip(confidence, 0, 1)),
            "analysis_level": "light",
            "features_extra": extra,
            **self._extract_light_features(audio, sample_rate=light_dsp.SAMPLE_RATE),
        }
        raw_year = getattr(metadata, "year", None)
        year_prefix = str(raw_year).strip()[:4] if raw_year else ""
        if year_prefix.isdigit():
            result["year"] = int(year_prefix)

        return result

    def analyze_selected(self, filepath, features):
        # The background embedding job keeps the light profile's bounded ONNX
        # windows: no librosa import and no 44.1 kHz whole-track decode.
        if list(features) == ["embedding"]:
            duration = len(self._load_grid_audio(filepath)) / light_dsp.SAMPLE_RATE
            result = self._extract_light_embedding(filepath, duration)
            result["features_extra"]["analysis_components"] = {
                "embedding": constants.COMPONENT_VERSIONS["embedding"]}
            return result
        return super().analyze_selected(filepath, features)

    def _rhythm(self, audio):
        y = self.librosa.resample(audio, orig_sr=constants.SAMPLE_RATE, target_sr=22050)
        envelope = self.librosa.onset.onset_strength(y=y, sr=22050, hop_length=256)
        tempo, frames = self.librosa.beat.beat_track(onset_envelope=envelope, sr=22050, hop_length=256)
        ticks = self.librosa.frames_to_time(frames, sr=22050, hop_length=256)
        bpm = float(np.asarray(tempo).reshape(-1)[0])
        intervals = np.diff(ticks)
        confidence = float(np.clip(1 - np.std(intervals) / max(np.mean(intervals), 1e-8), 0, 1)) if intervals.size else 0.0
        return bpm, ticks, confidence, np.array([]), intervals

    def _key(self, audio):
        y = self.librosa.resample(audio, orig_sr=constants.SAMPLE_RATE, target_sr=22050)
        chroma = self.librosa.feature.chroma_stft(y=y, sr=22050).mean(axis=1)
        major = np.array([6.35,2.23,3.48,2.33,4.38,4.09,2.52,5.19,2.39,3.66,2.29,2.88])
        minor = np.array([6.33,2.68,3.52,5.38,2.60,3.53,2.54,4.75,3.98,2.69,3.34,3.17])
        centered = chroma - chroma.mean()
        scores = []
        for profile in (major, minor):
            profile = profile - profile.mean()
            scores.extend(float(np.dot(centered, np.roll(profile, key)) / max(np.linalg.norm(centered)*np.linalg.norm(profile), 1e-8)) for key in range(12))
        best = int(np.argmax(scores))
        return ["C","C#","D","Eb","E","F","F#","G","Ab","A","Bb","B"][best % 12], "major" if best < 12 else "minor", max(0.0, scores[best])

    def _extract_loudest_section(self, audio, duration_sec):
        count = constants.SAMPLE_RATE * duration_sec
        if len(audio) <= count:
            return audio
        hop = constants.SAMPLE_RATE
        rms = np.array([np.sqrt(np.mean(audio[i:i+2048]**2)) for i in range(0, len(audio), hop)])
        start = int(np.argmax(np.convolve(rms, np.ones(duration_sec), mode="valid"))) * hop
        return audio[start:start+count]

    def _get_embedding_session(self):
        if self._embedding_session is None:
            import onnxruntime as ort
            root = Path(getattr(sys, "_MEIPASS", Path(__file__).resolve().parents[3]))
            model = root / "models" / "msd-musicnn-1.onnx"
            options = ort.SessionOptions()
            options.intra_op_num_threads = 1
            options.inter_op_num_threads = 1
            options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
            self._embedding_session = ort.InferenceSession(
                str(model), sess_options=options, providers=["CPUExecutionProvider"])
        return self._embedding_session

    def _extract_light_embedding(self, filepath, duration):
        report("embedding", "類似曲検索用のモデルを準備しています")
        session = self._get_embedding_session()
        # Three ~3-second windows, evaluated one at a time to bound activation
        # memory. This is genuine MusiCNN inference, not a placeholder vector.
        starts = sorted(set(round(max(0, min(duration - 3, duration * fraction - 1.5)), 3)
                            for fraction in (.2, .5, .8)))
        vectors = []
        for index, start in enumerate(starts):
            report("embedding", f"類似曲検索用の特徴を解析しています（{index + 1} / {len(starts)}区間）")
            audio = self._load_audio_segment(filepath, start, 3, sample_rate=16000)
            bands = musicnn_bands(audio)
            if len(bands) < 187:
                bands = np.pad(bands, ((0, 187 - len(bands)), (0, 0)), mode="wrap")
            vectors.append(session.run(["embeddings"], {"melspectrogram": bands[None, :187]})[0][0])
        result = self._embedding_result(np.mean(vectors, axis=0))
        result["features_extra"].update(embedding_windows_seconds=starts, embedding_patch_count=len(starts),
                                        embedding_window_seconds=3)
        return result

    def _extract_embedding(self, audio):
        session = self._get_embedding_session()
        section = self._extract_loudest_section(audio, 60)
        section = self.librosa.resample(section, orig_sr=constants.SAMPLE_RATE, target_sr=16000)
        bands = musicnn_bands(section)
        if len(bands) < 187:
            bands = np.pad(bands, ((0,187-len(bands)),(0,0)), mode="wrap")
        patches = np.stack([bands[i:i+187] for i in range(0,len(bands)-186,93)])
        predictions = session.run(
            ["embeddings"], {"melspectrogram": patches})[0]
        return self._embedding_result(np.mean(predictions, axis=0))

    @staticmethod
    def _embedding_result(vector):
        if vector.shape != (200,) or not np.isfinite(vector).all() or not np.linalg.norm(vector):
            raise ValueError("MusiCNN produced an invalid embedding")
        return {"embedding": vector.tolist(), "embedding_model": constants.EMBEDDING_MODEL,
                "features_extra": {"embedding_sample_rate":16000, "embedding_pipeline_version":constants.EMBEDDING_PIPELINE_VERSION}}

    def _extract_timbre_features(self, audio):
        spectrum = np.abs(self.librosa.stft(audio, n_fft=2048, hop_length=1024))
        envelope = self.librosa.onset.onset_strength(S=self.librosa.amplitude_to_db(spectrum))
        correlation = self.librosa.autocorrelate(envelope, max_size=min(128,len(envelope)))
        periodicity = float(np.max(correlation[4:])/max(correlation[0],1e-8)) if len(correlation)>4 else 0.0
        return {"energy":float(np.sqrt(np.mean(audio**2))), "danceability":2.5*np.clip(periodicity,0,1),
                "brightness":float(self.librosa.feature.spectral_centroid(S=spectrum, sr=constants.SAMPLE_RATE).mean()),
                "noisiness":float(self.librosa.feature.zero_crossing_rate(audio).mean())}
