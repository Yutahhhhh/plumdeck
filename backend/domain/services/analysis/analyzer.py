import os
import sys
import logging
import numpy as np
from typing import Optional, Dict, Any, Tuple, List, Union
from tinytag import TinyTag
from . import constants
from .beat_grid import playback_grid, VERSION as GRID_VERSION
from .beat_alignment import align_beats
from .progress import report

# Windows uses ``PortableAudioAnalyzer`` for both profiles.  Importing
# Essentia here used to map its TensorFlow runtime before ``__new__`` could
# select that implementation.  A light-analysis worker consequently paid the
# detailed-analysis startup and memory cost on every track, which is enough to
# hit the short worker deadline on low-spec Windows machines.
#
# Keep the native Essentia import on macOS/Linux only.  Methods that reference
# ``es`` are overridden by the portable implementation on Windows.
if sys.platform != "win32":
    try:
        import essentia
        import essentia.standard as es
        HAS_ESSENTIA = True
    except ImportError:
        HAS_ESSENTIA = False
else:
    HAS_ESSENTIA = False

logger = logging.getLogger(__name__)

class AudioAnalyzer:
    def __new__(cls):
        if cls is AudioAnalyzer and sys.platform == "win32":
            from .portable import PortableAudioAnalyzer
            return object.__new__(PortableAudioAnalyzer)
        return object.__new__(cls)

    def __init__(self):
        if not HAS_ESSENTIA:
            raise ImportError("Essentia not found")
        self._init_algorithms()

    def _init_algorithms(self):
        self.rhythm_extractor = es.RhythmExtractor2013(method=constants.RHYTHM_METHOD)
        self.key_extractor = es.KeyExtractor(profileType=constants.KEY_PROFILE_TYPE)
        self.rms_algo = es.RMS()
        self.danceability_algo = es.Danceability()
        self.centroid_algo = es.SpectralCentroidTime()
        self.zcr_algo = es.ZeroCrossingRate()
        self.embedding_algo = None
        
        self._embedding_initialized = False

    def _load_embedding_model(self):
        if getattr(self, "_embedding_initialized", False):
            return
        self._embedding_initialized = True
        if getattr(sys, 'frozen', False):
            base_dir = sys._MEIPASS
        else:
            base_dir = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
        model_path = os.path.join(base_dir, "models", "msd-musicnn-1.pb")
        
        if os.path.exists(model_path):
            try:
                self.embedding_algo = es.TensorflowPredictMusiCNN(graphFilename=model_path, output="model/dense/BiasAdd")
            except Exception as e:
                logger.warning(f"Failed to load MusiCNN: {e}")

    def analyze(self, filepath: str, skip_basic: bool = False, skip_waveform: bool = False,
                external_lyrics: Optional[str] = None,
                skip_components: Tuple[str, ...] = ()) -> Optional[Dict[str, Any]]:
        """Full analysis. ``skip_components`` ('rhythm', 'key') are values the
        caller already has from rekordbox; they are left out of the result."""
        filename = os.path.basename(filepath)

        try:
            report("metadata", "曲名・長さなどの音源情報を読み取っています")
            tag = self._extract_metadata(filepath) or TinyTag(None, 0)
            report("decode", "音源全体を読み込んでいます")
            audio = self._load_audio(filepath)
            if audio is None: return None

            result = {}
            if not skip_basic:
                features = self._extract_features(audio, skip_components)
                # 確実に引数を渡す
                result = self._format_result(filepath, tag, features, external_lyrics=external_lyrics)
            else:
                # skip_basic時の辞書構築 (NameErrorを防止)
                year_val = None
                if tag.year:
                    try: year_val = int(str(tag.year).strip()[:4])
                    except: pass
                
                # 歌詞の特定
                lyrics_final = external_lyrics if external_lyrics else (tag.extra.get('lyrics') if hasattr(tag, 'extra') else None)
                
                result = {
                    "filepath": filepath,
                    "title": (tag.title or "").strip() or os.path.splitext(filename)[0],
                    "artist": (tag.artist or "").strip() or "Unknown",
                    "album": (tag.album or "").strip() or "Unknown",
                    "year": year_val,
                    "duration": tag.duration or 0.0,
                    "bpm": 0.0,
                    "key": "",
                    "lyrics": lyrics_final,
                    "features_extra": {}
                }

            if not skip_waveform:
                report("waveform", "波形データを作成しています")
                peaks = self._compute_waveform_peaks(audio, num_points=500)
                if "features_extra" not in result: result["features_extra"] = {}
                result["features_extra"]["waveform_peaks"] = peaks

            # Similarity is filled by a durable background job after import.
            result["analysis_level"] = "full"
            result.setdefault("features_extra", {}).update(
                analysis_level="full", analysis_profile="full"
            )

            return result
        except Exception as e:
            print(f"ERROR processing {filepath}: {e}", flush=True)
            return None

    def _extract_embedding(self, audio: np.ndarray) -> Dict[str, Any]:
        self._load_embedding_model()
        if self.embedding_algo is None:
            raise RuntimeError("MusiCNN model is unavailable")
        section = self._extract_loudest_section(audio, 60)
        section = es.Resample(inputSampleRate=constants.SAMPLE_RATE,
                              outputSampleRate=constants.EMBEDDING_SAMPLE_RATE, quality=1)(section)
        embeddings = self.embedding_algo(section)
        if embeddings.ndim != 2 or not embeddings.size or not np.isfinite(embeddings).all():
            raise ValueError("MusiCNN returned invalid embeddings")
        return {
            "embedding": np.mean(embeddings, axis=0).tolist(),
            "embedding_model": constants.EMBEDDING_MODEL,
            "features_extra": {
                "embedding_sample_rate": constants.EMBEDDING_SAMPLE_RATE,
                "embedding_pipeline_version": constants.EMBEDDING_PIPELINE_VERSION,
            },
        }

    def analyze_selected(self, filepath: str, features: List[str]) -> Dict[str, Any]:
        """Read audio once and compute only requested components; never read/write tags."""
        if not features or set(features) - constants.COMPONENT_VERSIONS.keys():
            raise ValueError("Unknown or empty analysis components")
        audio = self._load_audio(filepath)
        if audio is None or not len(audio):
            raise ValueError("Audio could not be decoded")
        result: Dict[str, Any] = {"features_extra": {}}
        extra = result["features_extra"]
        if "embedding" in features:
            embedding = self._extract_embedding(audio)
            extra.update(embedding.pop("features_extra"))
            result.update(embedding)
        if "rhythm" in features:
            bpm, ticks, confidence, _, _ = self.rhythm_extractor(audio)
            ticks = align_beats(audio, ticks, constants.SAMPLE_RATE)
            result["bpm"] = round(float(bpm), 2)
            extra.update(bpm_confidence=float(confidence), beat_positions=ticks.tolist())
            extra.update(playback_grid=playback_grid(ticks, bpm, float(confidence)), playback_grid_version=GRID_VERSION)
        if "key" in features:
            key, scale, _ = self.key_extractor(audio)
            result.update(key=f"{key} {scale}", scale=scale)
        if "waveform" in features:
            extra["waveform_peaks"] = self._compute_waveform_peaks(audio, 500)
        if "timbre" in features:
            raw = self._extract_timbre_features(audio)
            def norm(value, bounds):
                return round(float(np.clip((value - bounds[0]) / (bounds[1] - bounds[0]), 0, 1)), 2)
            result.update(
                energy=norm(raw["energy"], constants.NORM_ENERGY),
                danceability=norm(raw["danceability"], constants.NORM_DANCEABILITY),
                brightness=norm(raw["brightness"], constants.NORM_BRIGHTNESS),
                noisiness=norm(raw["noisiness"], constants.NORM_NOISINESS),
            )
        extra["analysis_components"] = {f: constants.COMPONENT_VERSIONS[f] for f in features}
        return result

    def _extract_loudest_section(self, audio: np.ndarray, duration_sec: int) -> np.ndarray:
        sr = constants.SAMPLE_RATE
        target_samples = duration_sec * sr
        if len(audio) <= target_samples: return audio
        hop_size = sr
        rms_values = [self.rms_algo(frame) for frame in es.FrameGenerator(audio, frameSize=2048, hopSize=hop_size)]
        rms_array = np.array(rms_values).flatten()
        if len(rms_array) < duration_sec: return audio
        window = np.ones(int(duration_sec))
        energy_profile = np.convolve(rms_array, window, mode='valid')
        start_sample = np.argmax(energy_profile) * hop_size
        return audio[start_sample : start_sample + target_samples]

    def _compute_waveform_peaks(self, audio: np.ndarray, num_points: int = 2000) -> List[float]:
        if len(audio) == 0: return []
        if len(audio) <= num_points: return [round(float(abs(x)), 4) for x in audio]
        chunk_size = len(audio) // num_points
        reshaped = np.abs(audio[:chunk_size * num_points]).reshape(num_points, chunk_size)
        return [round(float(p), 4) for p in np.max(reshaped, axis=1)]

    def _extract_metadata(self, filepath: str) -> Optional[TinyTag]:
        try: return TinyTag.get(filepath)
        except: return None

    def _load_audio(self, filepath: str) -> Optional[np.ndarray]:
        try: return es.MonoLoader(filename=filepath, sampleRate=constants.SAMPLE_RATE)()
        except: return None

    def _extract_features(self, audio: np.ndarray, skip: Tuple[str, ...] = ()) -> Dict[str, Any]:
        features: Dict[str, Any] = {}
        if "rhythm" not in skip:
            report("rhythm", "BPM・ビート位置を解析しています")
            bpm, ticks, confidence, _, _ = self.rhythm_extractor(audio)
            ticks = align_beats(audio, ticks, constants.SAMPLE_RATE)
            features.update(bpm=bpm, beat_positions=ticks, bpm_confidence=confidence)
        if "key" not in skip:
            report("key", "キーを解析しています")
            key, scale, _ = self.key_extractor(audio)
            features.update(key=key, scale=scale)
        report("timbre", "音量・音色の詳細を解析しています")
        return {**features, **self._extract_timbre_features(audio)}

    def _extract_timbre_features(self, audio: np.ndarray) -> Dict[str, Any]:
        # Loudness, loudness range, rolloff, flux and contrast had no reader
        # (no auto-gain); only these four descriptors are stored.
        return {
            "energy": np.mean(self.rms_algo(audio)),
            "danceability": self.danceability_algo(audio)[0],
            "brightness": self.centroid_algo(audio),
            "noisiness": np.mean(self.zcr_algo(audio)),
        }

    def _format_result(self, filepath: str, tag: TinyTag, features: Dict[str, Any], external_lyrics: Optional[str] = None) -> Dict[str, Any]:
        def safe_s(v): return float(v) if isinstance(v, (np.number, float, int)) else v
        def norm(v, min_v, max_v): return max(0.0, min(1.0, (float(v) - min_v) / (max_v - min_v))) if max_v != min_v else 0.0

        # 正確な変数名で処理
        lyrics_val = external_lyrics if external_lyrics else (tag.extra.get('lyrics') if hasattr(tag, 'extra') else None)
        
        rhythm, key = "bpm" in features, "key" in features
        computed = [name for name, done in (("rhythm", rhythm), ("key", key), ("timbre", True)) if done]
        extra: Dict[str, Any] = {
            "analysis_components": {name: constants.COMPONENT_VERSIONS[name] for name in computed},
        }
        if rhythm:
            extra.update({
                "bpm_confidence": round(safe_s(features['bpm_confidence']), 2),
                "playback_grid": playback_grid(features['beat_positions'], features['bpm'], float(features['bpm_confidence'])),
                "playback_grid_version": GRID_VERSION,
                "beat_positions": features['beat_positions'].tolist() if hasattr(features['beat_positions'], 'tolist') else []
            })
        return {
            "filepath": filepath,
            "title": (tag.title or "").strip() or os.path.splitext(os.path.basename(filepath))[0],
            "artist": (tag.artist or "Unknown").strip(),
            "album": (tag.album or "Unknown").strip(),
            "genre": (tag.genre or "Unknown").strip(),
            "year": int(str(tag.year).strip()[:4]) if tag.year and str(tag.year).strip()[:4].isdigit() else None,
            "lyrics": lyrics_val,
            "duration": safe_s(tag.duration or 0.0),
            # Skipped components are filled from rekordbox by the caller.
            "bpm": round(float(features['bpm']), 2) if rhythm else 0.0,
            "key": f"{features['key']} {features['scale']}" if key else "",
            "scale": features['scale'] if key else "",
            "energy": round(norm(features['energy'], *constants.NORM_ENERGY), 2),
            "danceability": round(norm(features['danceability'], *constants.NORM_DANCEABILITY), 2),
            "brightness": round(norm(features['brightness'], *constants.NORM_BRIGHTNESS), 2),
            "noisiness": round(norm(features['noisiness'], *constants.NORM_NOISINESS), 2),
            "features_extra": extra,
        }
