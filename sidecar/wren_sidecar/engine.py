"""Model loading, transcription and the request handler.

Defaults are the benchmark's pick on Maximilian's own voice (docs/research/stt-benchmark.md):
large-v3-turbo, int8_float16, beam 1, language "en", VAD filter on (without it every model invented
text on silent clips), condition_on_previous_text off, timestamps on, dictionary terms as a
"Glossary: ..." initial_prompt (not hotwords).

Privacy: dictated text only ever leaves this module inside a `result`. Errors and log lines use fixed
wording plus exception *type names*, never exception text (which could quote the audio's words).
"""
from __future__ import annotations

import logging
import time
import wave
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

import numpy as np

from . import cuda, protocol

SAMPLE_RATE = 16000
# Clips shorter than this (10 ms) hold no speech; skip the model entirely.
MIN_SAMPLES = SAMPLE_RATE // 100

# Whisper keeps only ~220 prompt tokens; ~4 characters a token leaves room for the decoder's own.
MAX_PROMPT_CHARS = 800

Log = Callable[[str, str], None]

# faster-whisper's INFO lines are harmless (durations only), but nothing below WARNING is needed.
logging.getLogger("faster_whisper").setLevel(logging.WARNING)


class BadAudio(Exception):
    """The audio file is missing or unreadable. The message is fixed wording, safe to log."""


@dataclass
class EngineConfig:
    models_dir: Path
    model: str = "large-v3-turbo"
    compute: str = "int8_float16"
    beam: int = 1
    language: str = "en"
    vad: bool = True
    condition_on_previous_text: bool = False
    cpu_fallback: str = "small.en"
    # "auto" tries CUDA then the CPU fallback; "cpu" goes straight to the fallback.
    device: str = "auto"


def model_path(models_dir: Path, name: str) -> Path:
    """A model is a folder under models_dir (or an absolute folder). Never a hub name: no downloads."""
    p = Path(name)
    if p.is_absolute():
        return p
    return Path(models_dir) / name


def read_wav(path: str | Path) -> np.ndarray:
    """16 kHz mono float32. 16-bit PCM 16 kHz mono (what Wren writes) is read directly; anything else
    goes through faster-whisper's decoder, which resamples."""
    p = Path(path)
    if not p.is_file():
        raise BadAudio("audio file not found")
    try:
        with wave.open(str(p), "rb") as w:
            if w.getnchannels() == 1 and w.getsampwidth() == 2 and w.getframerate() == SAMPLE_RATE:
                raw = w.readframes(w.getnframes())
                return np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0
    except (wave.Error, EOFError):
        pass  # not plain PCM; try the general decoder
    except OSError as e:
        raise BadAudio(f"audio file unreadable ({type(e).__name__})") from e
    try:
        from faster_whisper import decode_audio

        return decode_audio(str(p), sampling_rate=SAMPLE_RATE)
    except Exception as e:  # noqa: BLE001 - PyAV raises many types
        raise BadAudio(f"audio file could not be decoded ({type(e).__name__})") from e


def build_prompt(bias_terms: list[str], prompt: str) -> str | None:
    """The initial_prompt: the optional sentence, then "Glossary: t1, t2, ...".

    Benchmark result (docs/research/stt-benchmark.md, spec §5): dictionary terms go ONLY into this
    Glossary prompt (name recall 80% -> 93%, no harm elsewhere), never into `hotwords`, which made
    the model append dictionary words to trailing silence. Terms that would push the prompt past
    MAX_PROMPT_CHARS are dropped from the end (the list arrives ranked, best first).
    """
    sentence = (prompt or "").strip()
    terms = [t.strip() for t in bias_terms if t and t.strip()]
    budget = MAX_PROMPT_CHARS - len(sentence) - len("Glossary: .") - 1
    kept: list[str] = []
    for t in terms:
        cost = len(t) + (2 if kept else 0)
        if cost > budget:
            break
        kept.append(t)
        budget -= cost
    parts = [sentence] if sentence else []
    if kept:
        parts.append("Glossary: " + ", ".join(kept) + ".")
    return " ".join(parts) or None


def is_oom(e: BaseException) -> bool:
    s = str(e).lower()
    return "out of memory" in s or "cuda_error_out_of_memory" in s


class Transcriber:
    def __init__(self, model: Any, cfg: EngineConfig, *, name: str, device: str, compute: str):
        self.model = model
        self.cfg = cfg
        self.name = name
        self.device = device
        self.compute = compute
        self.vram_mb = 0

    def transcribe_array(self, audio: np.ndarray, bias_terms: list[str], prompt: str) -> dict[str, Any]:
        t0 = time.perf_counter()
        if audio.shape[0] < MIN_SAMPLES:
            return {"text": "", "speech_ms": 0, "infer_ms": 0, "segments": []}
        kwargs: dict[str, Any] = dict(
            language=self.cfg.language or None,
            beam_size=max(1, int(self.cfg.beam)),
            vad_filter=self.cfg.vad,
            condition_on_previous_text=self.cfg.condition_on_previous_text,
        )
        initial_prompt = build_prompt(bias_terms, prompt)
        if initial_prompt:
            kwargs["initial_prompt"] = initial_prompt  # never `hotwords`: see build_prompt
        segments_iter, info = self.model.transcribe(audio, **kwargs)
        segments = []
        # The generator does the decoding; consume it inside the timing.
        for s in segments_iter:
            segments.append(
                {
                    "start": round(float(s.start), 3),
                    "end": round(float(s.end), 3),
                    "text": s.text.strip(),
                    "no_speech_prob": round(float(s.no_speech_prob), 4),
                }
            )
        infer_ms = int((time.perf_counter() - t0) * 1000)
        speech_s = float(getattr(info, "duration_after_vad", None) or 0.0) if self.cfg.vad else float(info.duration)
        text = " ".join(s["text"] for s in segments if s["text"]).strip()
        return {"text": text, "speech_ms": int(round(speech_s * 1000)), "infer_ms": infer_ms, "segments": segments}

    def transcribe_file(self, path: str | Path, bias_terms: list[str], prompt: str) -> dict[str, Any]:
        return self.transcribe_array(read_wav(path), bias_terms, prompt)


def _load(cfg: EngineConfig, name: str, device: str, compute: str) -> Transcriber:
    if device == "cuda":
        cuda.add_dll_directories()
    folder = model_path(cfg.models_dir, name)
    if not (folder / "model.bin").is_file():
        raise FileNotFoundError(f"model {name} is not installed")
    from faster_whisper import WhisperModel

    # local_files_only: a missing model fails loudly here instead of being fetched from the internet.
    model = WhisperModel(str(folder), device=device, compute_type=compute, local_files_only=True)
    return Transcriber(model, cfg, name=name, device=device, compute=compute)


def load_transcriber(cfg: EngineConfig, log: Log, warmup_wav: Path | None = None) -> Transcriber:
    """CUDA first (unless cfg.device == "cpu"), then the CPU fallback model, int8. Warm-up runs on
    whichever loaded, so a CUDA failure that only shows at the first decode also falls back."""
    reason = "CPU requested"
    if cfg.device != "cpu":
        reason = cuda.cuda_problem() or ""
        if not reason:
            before = cuda.gpu_used_mb()
            t0 = time.perf_counter()
            try:
                t = _load(cfg, cfg.model, "cuda", cfg.compute)
                load_ms = int((time.perf_counter() - t0) * 1000)
                warm_ms = warmup(t, warmup_wav)
                after = cuda.gpu_used_mb()
                t.vram_mb = max(0, after - before) if before is not None and after is not None else 0
                log("info", f"loaded {cfg.model} on cuda ({cfg.compute}) in {load_ms} ms, warm-up {warm_ms} ms")
                return t
            except FileNotFoundError:
                reason = f"model {cfg.model} is not installed"
            except Exception as e:  # noqa: BLE001
                reason = "out of GPU memory" if is_oom(e) else f"CUDA load failed ({type(e).__name__})"
    log("warn", f"using {cfg.cpu_fallback} on CPU: {reason}")
    t0 = time.perf_counter()
    t = _load(cfg, cfg.cpu_fallback, "cpu", "int8")
    load_ms = int((time.perf_counter() - t0) * 1000)
    warm_ms = warmup(t, warmup_wav)
    log("info", f"loaded {cfg.cpu_fallback} on cpu (int8) in {load_ms} ms, warm-up {warm_ms} ms")
    return t


def warmup(t: Transcriber, wav: Path | None) -> int:
    """Run one real transcription so the first dictation does not pay for kernel set-up."""
    t0 = time.perf_counter()
    if wav is not None and Path(wav).is_file():
        audio = read_wav(wav)
    else:
        # No bundled clip: one second of low noise still exercises the decoder.
        audio = (np.random.default_rng(0).standard_normal(SAMPLE_RATE) * 0.01).astype(np.float32)
    t.transcribe_array(audio, [], "")
    return int((time.perf_counter() - t0) * 1000)


class Handler:
    """Turns one parsed request into the messages to send back. No I/O of its own."""

    def __init__(self, transcriber: Transcriber | None):
        self.transcriber = transcriber

    def handle(self, msg: dict[str, Any]) -> list[dict[str, Any]]:
        kind = msg["type"]
        if kind == "ping":
            return [protocol.pong()]
        if kind in ("pong", "log", "ready", "result", "error"):
            return []  # nothing to answer
        if kind == "transcribe":
            return [self._transcribe(msg)]
        return []  # shutdown is handled by the caller

    def _transcribe(self, msg: dict[str, Any]) -> dict[str, Any]:
        rid = msg["id"]
        if self.transcriber is None:
            return protocol.error(rid, "NOT_READY", "model is still loading")
        try:
            r = self.transcriber.transcribe_file(msg["audio_path"], msg.get("bias_terms") or [], msg.get("prompt") or "")
        except BadAudio as e:
            return protocol.error(rid, "BAD_AUDIO", str(e))
        except Exception as e:  # noqa: BLE001
            if is_oom(e):
                return protocol.error(rid, "OOM", "out of GPU memory")
            return protocol.error(rid, "INTERNAL", f"transcription failed ({type(e).__name__})")
        return protocol.result(rid, **r)
