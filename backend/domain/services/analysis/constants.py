
# Audio Analysis Constants

# Audio Loading
# Keep 44.1kHz to preserve high-frequency content (air, brilliance) for high-quality timbre analysis.
SAMPLE_RATE = 44100
EMBEDDING_SAMPLE_RATE = 16000
EMBEDDING_PIPELINE_VERSION = "musicnn-16khz-v1"
EMBEDDING_MODEL = "msd-musicnn-1:musicnn-16khz-v1"
COMPONENT_VERSIONS = {
    "embedding": EMBEDDING_PIPELINE_VERSION,
    "rhythm": "rhythm-attacks-v4",
    "key": "key-v1",
    "timbre": "timbre-v1",
    "waveform": "waveform-v1",
}

# Analysis Parameters
KEY_PROFILE_TYPE = 'edma'
RHYTHM_METHOD = 'multifeature'

# Normalization Ranges (Min, Max)
# These are heuristic values based on typical Essentia outputs
NORM_ENERGY = (0.0, 0.4)          # Slightly increased max to prevent ceiling effect (was 0.3)
NORM_DANCEABILITY = (0.0, 2.5)
NORM_BRIGHTNESS = (500.0, 5000.0)
NORM_NOISINESS = (0.0, 0.15)

# Different DSP implementations must not claim each other's analysis version.
import sys
if sys.platform == "win32":
    COMPONENT_VERSIONS.update(rhythm="librosa-attacks-v2", key="librosa-key-v1", timbre="librosa-timbre-v1")
GRID_COMPONENT_VERSION = "numpy-attacks-v2" if sys.platform == "win32" else COMPONENT_VERSIONS["rhythm"]
