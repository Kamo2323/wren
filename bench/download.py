"""Download the candidate models into bench/models/ and print each one's size on disk.

Only official / well-known Hugging Face repos are used. Re-running skips files already present.
Usage: .venv/Scripts/python.exe download.py [name ...]
"""
import sys
from pathlib import Path

from huggingface_hub import snapshot_download

MODELS = Path(__file__).parent / "models"

# name -> (repo, allow_patterns or None)
REPOS = {
    "large-v3-turbo": ("mobiuslabsgmbh/faster-whisper-large-v3-turbo", None),  # faster-whisper's own alias
    "distil-large-v3.5": ("distil-whisper/distil-large-v3.5-ct2", None),       # official distil-whisper org
    "small.en": ("Systran/faster-whisper-small.en", None),                     # faster-whisper's own alias
    "parakeet-tdt-0.6b-v2": ("istupakov/parakeet-tdt-0.6b-v2-onnx", None),     # onnx-asr's default export
    "silero-vad": ("istupakov/silero-vad-onnx", None),                         # onnx-asr's default VAD
}


def size_mb(p: Path) -> float:
    return sum(f.stat().st_size for f in p.rglob("*") if f.is_file() and ".cache" not in f.parts) / 2**20


def main(names):
    for name in names or REPOS:
        repo, patterns = REPOS[name]
        dest = MODELS / name
        path = snapshot_download(repo, local_dir=dest, allow_patterns=patterns)
        print(f"{name:24s} {repo:45s} {size_mb(Path(path)):8.0f} MB")


if __name__ == "__main__":
    main(sys.argv[1:])
