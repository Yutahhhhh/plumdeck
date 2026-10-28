import os
import logging
import threading
from domain.services.analysis.progress import report
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from domain.services.analysis.analyzer import AudioAnalyzer

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)

# Thread-local storage for AudioAnalyzer instances
# This allows us to reuse Analyzer instances within the same thread,
# avoiding the overhead and logs of repeated initialization,
# while maintaining thread safety (Essentia algorithms are not thread-safe).
_thread_local = threading.local()

def get_analyzer() -> Optional["AudioAnalyzer"]:
    if not hasattr(_thread_local, "analyzer"):
        try:
            # Loading Essentia also maps its TensorFlow dependency. Keep this
            # inside the analysis worker, not the Play/library startup path.
            from domain.services.analysis.analyzer import AudioAnalyzer

            _thread_local.analyzer = AudioAnalyzer()
        except ImportError:
            _thread_local.analyzer = None
        except Exception as e:
            logger.error(f"Error initializing AudioAnalyzer: {e}")
            _thread_local.analyzer = None
    return _thread_local.analyzer

def analyze_track_file(filepath: str, high_precision: bool = True, skip_basic: bool = False,
                       skip_waveform: bool = False, external_lyrics: Optional[str] = None,
                       analysis_profile: str = "full") -> Optional[dict]:
    """
    Wrapper function for backward compatibility.

    ``high_precision`` is ignored. It stays in the signature because workers
    receive these arguments positionally (ingestion_domain_service.run_args).
    """
    report("initializing", "解析ワーカーを初期化しています")
    analyzer = get_analyzer()
    if analyzer:
        if analysis_profile == "light" and hasattr(analyzer, "analyze_light"):
            return analyzer.analyze_light(filepath, external_lyrics=external_lyrics)
        return analyzer.analyze(filepath, skip_basic=skip_basic, skip_waveform=skip_waveform, external_lyrics=external_lyrics)
    return None
