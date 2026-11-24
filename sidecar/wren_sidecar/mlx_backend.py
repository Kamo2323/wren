"""Apple Silicon MLX Whisper backend for macOS.

Uses mlx-whisper (Metal GPU) for transcription + faster-whisper's bundled ONNX Silero VAD for
identical silence filtering and speech duration measurement as the Windows CUDA backend.

Memory discipline for 8 GB unified-memory Macs:
- Caps MLX's Metal buffer cache (`MLX_CACHE_LIMIT_BYTES`) so intermediate decode buffers never
  accumulate and push macOS into swap.
- Calls `mx.clear_cache()` after each transcription so idle footprint stays strictly at the
  quantized model weights (~450-650 MB for large-v3-turbo-q4).
"""
from __future__ import annotations

import platform
import sys
import time
from pathlib import Path
from typing import Any, Callable

import numpy as np

SAMPLE_RATE = 16000
MIN_SAMPLES = SAMPLE_RATE // 100
# Cap Metal allocator cache at 128 MiB to keep total unified memory lean on 8 GB Macs.
MLX_CACHE_LIMIT_BYTES = 128 * 1024 * 1024


def is_apple_silicon() -> bool:
    return sys.platform == "darwin" and platform.machine().lower() in ("arm64", "aarch64")


def mlx_problem() -> str | None:
    """Return a short reason why MLX cannot be used here, or None if Apple Silicon MLX is ready."""
    if not is_apple_silicon():
        return "not Apple Silicon macOS"
    try:
        import mlx.core as mx
        import mlx_whisper  # noqa: F401
    except Exception as e:  # noqa: BLE001
        return f"mlx-whisper unavailable ({type(e).__name__})"
    try:
        if not mx.metal.is_available():
            return "Metal GPU unavailable"
    except Exception:  # noqa: BLE001
        pass
    return None


def has_mlx_weights(folder: Path) -> bool:
    return (folder / "config.json").is_file() and (
        (folder / "weights.npz").is_file() or (folder / "weights.safetensors").is_file()
    )


def resolve_mlx_model_dir(models_dir: Path, name: str) -> Path:
    """Resolve an MLX model directory strictly on disk (never downloads).

    If `name` is e.g. `large-v3-turbo` (such as from a synced Windows settings.json) and only
    `large-v3-turbo-q4` is installed locally, fall back to the `-q4` sibling cleanly.
    """
    p = Path(name)
    direct = p if p.is_absolute() else Path(models_dir) / name
    if has_mlx_weights(direct):
        return direct
    if not p.is_absolute() and not name.endswith("-q4"):
        q4 = Path(models_dir) / f"{name}-q4"
        if has_mlx_weights(q4):
            return q4
    raise FileNotFoundError(f"MLX model {name} is not installed")


def _run_silero_vad(audio: np.ndarray) -> tuple[np.ndarray, int]:
    """Filter audio with faster-whisper's bundled ONNX Silero VAD.

    Returns `(speech_audio, speech_ms)`. If no speech is found, `speech_audio` is empty and
    `speech_ms` is 0 so the Whisper decoder never runs on silence.
    """
    from faster_whisper.vad import collect_chunks, get_speech_timestamps

    chunks = get_speech_timestamps(audio, sampling_rate=SAMPLE_RATE)
    if not chunks:
        return np.empty(0, dtype=np.float32), 0
    speech_samples = sum(max(0, int(c["end"]) - int(c["start"])) for c in chunks)
    speech_ms = int(round((speech_samples / SAMPLE_RATE) * 1000))
    if speech_samples < MIN_SAMPLES:
        return np.empty(0, dtype=np.float32), speech_ms
    audio_chunks, _ = collect_chunks(audio, chunks, sampling_rate=SAMPLE_RATE)
    if not audio_chunks or audio_chunks[0].size < MIN_SAMPLES:
        return np.empty(0, dtype=np.float32), speech_ms
    return np.asarray(audio_chunks[0], dtype=np.float32), speech_ms


class MlxTranscriber:
    """Drop-in equivalent of `engine.Transcriber` backed by Apple Silicon MLX."""

    def __init__(
        self,
        model_dir: Path,
        cfg: Any,
        *,
        name: str,
        compute: str,
        build_prompt_fn: Callable[[list[str], str, Callable[[str], int]], str | None],
        read_wav_fn: Callable[[str | Path], np.ndarray],
        estimate_tokens_fn: Callable[[str], int],
    ):
        import mlx.core as mx
        import mlx_whisper  # noqa: F401
        from mlx_whisper.tokenizer import get_tokenizer

        # Load and keep the model warm in mlx_whisper's ModelHolder.
        mt = sys.modules["mlx_whisper.transcribe"]
        try:
            mx.set_cache_limit(MLX_CACHE_LIMIT_BYTES)
        except Exception:  # noqa: BLE001
            pass

        self._mx = mx
        self._transcribe = mlx_whisper.transcribe
        self._model_path = str(model_dir)
        self.model = mt.ModelHolder.get_model(self._model_path, mx.float16)
        self.cfg = cfg
        self.name = name
        self.device = "mlx"
        self.compute = compute
        self.engine_name = "mlx-whisper"
        self._build_prompt = build_prompt_fn
        self._read_wav = read_wav_fn

        try:
            active = int(mx.get_active_memory())
            self.vram_mb = max(1, int(round(active / (1024 * 1024))))
        except Exception:  # noqa: BLE001
            self.vram_mb = 0

        try:
            is_multi = bool(getattr(self.model, "is_multilingual", True))
            num_langs = int(getattr(self.model, "num_languages", 99))
            tok = get_tokenizer(
                multilingual=is_multi,
                num_languages=num_langs,
                language=(cfg.language or "en") if is_multi else None,
                task="transcribe",
            )
            self.count_tokens: Callable[[str], int] = lambda text: len(tok.encode(" " + text.strip()))
        except Exception:  # noqa: BLE001
            self.count_tokens = estimate_tokens_fn

    def transcribe_array(self, audio: np.ndarray, bias_terms: list[str], prompt: str) -> dict[str, Any]:
        t0 = time.perf_counter()
        audio = np.asarray(audio, dtype=np.float32).reshape(-1)
        if audio.shape[0] < MIN_SAMPLES:
            return {"text": "", "speech_ms": 0, "infer_ms": 0, "segments": []}

        if self.cfg.vad:
            speech_audio, speech_ms = _run_silero_vad(audio)
            if speech_audio.shape[0] < MIN_SAMPLES:
                infer_ms = int((time.perf_counter() - t0) * 1000)
                return {"text": "", "speech_ms": speech_ms, "infer_ms": infer_ms, "segments": []}
        else:
            speech_audio = audio
            speech_ms = int(round((audio.shape[0] / SAMPLE_RATE) * 1000))

        kwargs: dict[str, Any] = dict(
            path_or_hf_repo=self._model_path,
            # `verbose=None` disables both stdout segment printing and tqdm progress bars in mlx_whisper.
            verbose=None,
            fp16=True,
            temperature=0.0,
            condition_on_previous_text=self.cfg.condition_on_previous_text,
        )
        if self.cfg.language:
            kwargs["language"] = self.cfg.language
        initial_prompt = self._build_prompt(bias_terms, prompt, self.count_tokens)
        if initial_prompt:
            kwargs["initial_prompt"] = initial_prompt

        try:
            out = self._transcribe(speech_audio, **kwargs)
        finally:
            try:
                self._mx.clear_cache()
            except Exception:  # noqa: BLE001
                pass

        segments = []
        for s in out.get("segments") or []:
            segments.append(
                {
                    "start": round(float(s.get("start", 0.0)), 3),
                    "end": round(float(s.get("end", 0.0)), 3),
                    "text": str(s.get("text", "")).strip(),
                    "no_speech_prob": round(float(s.get("no_speech_prob", 0.0)), 4),
                }
            )
        infer_ms = int((time.perf_counter() - t0) * 1000)
        text = " ".join(s["text"] for s in segments if s["text"]).strip()
        if not text:
            text = str(out.get("text", "")).strip()
        return {"text": text, "speech_ms": speech_ms, "infer_ms": infer_ms, "segments": segments}

    def transcribe_file(self, path: str | Path, bias_terms: list[str], prompt: str) -> dict[str, Any]:
        return self.transcribe_array(self._read_wav(path), bias_terms, prompt)
