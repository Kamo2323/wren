"""Protocol v1: JSON lines over stdio, one object per line, UTF-8 (spec §5).

app -> sidecar: transcribe, ping, shutdown
sidecar -> app: ready, result, error, pong, log (and ping/shutdown are accepted both ways)

Rule: no message except `result` ever carries dictated text. `log` and `error` messages are written by
this package only, with fixed wording.
"""
from __future__ import annotations

import json
from typing import Any

VERSION = 1
ENGINE = "faster-whisper"
ERROR_CODES = ("OOM", "BAD_AUDIO", "NOT_READY", "INTERNAL")
LOG_LEVELS = ("info", "warn", "error")
TYPES = ("ready", "transcribe", "result", "error", "ping", "pong", "shutdown", "log")


class ProtocolError(ValueError):
    """A line that is not a valid v1 message. The message never quotes the line itself."""


def encode(msg: dict[str, Any]) -> str:
    """One line, newline-terminated. Non-ASCII stays UTF-8; newlines inside strings are escaped."""
    return json.dumps(msg, ensure_ascii=False, separators=(",", ":")) + "\n"


def parse(line: str | bytes) -> dict[str, Any]:
    if isinstance(line, bytes):
        try:
            line = line.decode("utf-8")
        except UnicodeDecodeError as e:
            raise ProtocolError("line is not UTF-8") from e
    try:
        msg = json.loads(line)
    except json.JSONDecodeError as e:
        raise ProtocolError("line is not JSON") from e
    if not isinstance(msg, dict):
        raise ProtocolError("message is not an object")
    if msg.get("v") != VERSION:
        raise ProtocolError("unsupported protocol version")
    kind = msg.get("type")
    if kind not in TYPES:
        raise ProtocolError("unknown message type")
    if kind == "transcribe":
        if not isinstance(msg.get("id"), str) or not msg["id"]:
            raise ProtocolError("transcribe needs an id")
        if not isinstance(msg.get("audio_path"), str) or not msg["audio_path"]:
            raise ProtocolError("transcribe needs an audio_path")
        terms = msg.get("bias_terms") or []
        if not isinstance(terms, list) or not all(isinstance(t, str) for t in terms):
            raise ProtocolError("bias_terms must be a list of strings")
        msg["bias_terms"] = terms
        prompt = msg.get("prompt") or ""
        if not isinstance(prompt, str):
            raise ProtocolError("prompt must be a string")
        msg["prompt"] = prompt
    return msg


# ------------------------------------------------------------------------------------ constructors


def ready(*, model: str, device: str, compute: str, vram_mb: int, engine: str = ENGINE) -> dict[str, Any]:
    return {
        "v": VERSION,
        "type": "ready",
        "engine": engine,
        "model": model,
        "device": device,
        "compute": compute,
        "vram_mb": int(vram_mb),
        # hotwords False: bias terms travel only as a Glossary prompt (benchmark, spec §5).
        "caps": {"hotwords": False, "prompt": True, "vad": True},
    }


def result(id: str, *, text: str, speech_ms: int, infer_ms: int, segments: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "v": VERSION,
        "type": "result",
        "id": id,
        "text": text,
        "speech_ms": int(speech_ms),
        "infer_ms": int(infer_ms),
        "segments": segments,
    }


def error(id: str | None, code: str, msg: str) -> dict[str, Any]:
    if code not in ERROR_CODES:
        raise ValueError(f"unknown error code {code!r}")
    return {"v": VERSION, "type": "error", "id": id, "code": code, "msg": msg}


def log(level: str, msg: str) -> dict[str, Any]:
    if level not in LOG_LEVELS:
        raise ValueError(f"unknown log level {level!r}")
    return {"v": VERSION, "type": "log", "level": level, "msg": msg}


def ping() -> dict[str, Any]:
    return {"v": VERSION, "type": "ping"}


def pong() -> dict[str, Any]:
    return {"v": VERSION, "type": "pong"}


def shutdown() -> dict[str, Any]:
    return {"v": VERSION, "type": "shutdown"}
