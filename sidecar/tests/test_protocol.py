"""Protocol v1 (spec §5) and the request handler.

Pure message tests always run. The handler tests that transcribe need a model folder: they use
WREN_MODELS_DIR (default %LOCALAPPDATA%\\Wren\\models) and WREN_TEST_MODEL (default the shipped
default, large-v3-turbo), and are skipped if it is not there.
"""
import json
import os
import wave
from pathlib import Path

import pytest

from wren_sidecar import protocol
from wren_sidecar.protocol import ProtocolError

# ------------------------------------------------------------------------------------------ messages


def roundtrip(msg: dict) -> dict:
    line = protocol.encode(msg)
    assert line.endswith("\n") and line.count("\n") == 1, "one object per line"
    return protocol.parse(line)


def test_ready_roundtrip():
    msg = protocol.ready(model="large-v3-turbo", device="cuda", compute="int8_float16", vram_mb=1400)
    assert msg == {
        "v": 1,
        "type": "ready",
        "engine": "faster-whisper",
        "model": "large-v3-turbo",
        "device": "cuda",
        "compute": "int8_float16",
        "vram_mb": 1400,
        "caps": {"hotwords": True, "prompt": True, "vad": True},
    }
    assert roundtrip(msg) == msg


def test_transcribe_parses():
    line = json.dumps(
        {"v": 1, "type": "transcribe", "id": "d1", "audio_path": "C:\\a.wav", "bias_terms": ["Wren"], "prompt": "Hi."}
    )
    msg = protocol.parse(line)
    assert msg["id"] == "d1" and msg["audio_path"] == "C:\\a.wav"
    assert msg["bias_terms"] == ["Wren"] and msg["prompt"] == "Hi."
    assert roundtrip(msg) == msg


def test_transcribe_defaults_optional_fields():
    msg = protocol.parse('{"v":1,"type":"transcribe","id":"d2","audio_path":"x.wav"}')
    assert msg["bias_terms"] == [] and msg["prompt"] == ""


def test_transcribe_requires_id_and_path():
    with pytest.raises(ProtocolError):
        protocol.parse('{"v":1,"type":"transcribe","audio_path":"x.wav"}')
    with pytest.raises(ProtocolError):
        protocol.parse('{"v":1,"type":"transcribe","id":"d3"}')


def test_result_roundtrip():
    seg = {"start": 0.0, "end": 1.5, "text": "Hello there.", "no_speech_prob": 0.01}
    msg = protocol.result("d1", text="Hello there.", speech_ms=1200, infer_ms=210, segments=[seg])
    assert msg == {
        "v": 1,
        "type": "result",
        "id": "d1",
        "text": "Hello there.",
        "speech_ms": 1200,
        "infer_ms": 210,
        "segments": [seg],
    }
    assert roundtrip(msg) == msg


@pytest.mark.parametrize("code", ["OOM", "BAD_AUDIO", "NOT_READY", "INTERNAL"])
def test_error_roundtrip(code):
    msg = protocol.error("d1", code, "something")
    assert msg == {"v": 1, "type": "error", "id": "d1", "code": code, "msg": "something"}
    assert roundtrip(msg) == msg


def test_error_with_null_id():
    msg = protocol.error(None, "INTERNAL", "bad line")
    assert protocol.encode(msg).strip() == '{"v":1,"type":"error","id":null,"code":"INTERNAL","msg":"bad line"}'
    assert roundtrip(msg) == msg


def test_error_rejects_unknown_code():
    with pytest.raises(ValueError):
        protocol.error("d1", "NOPE", "x")


@pytest.mark.parametrize("kind", ["ping", "pong", "shutdown"])
def test_bare_messages(kind):
    msg = {"v": 1, "type": kind}
    assert getattr(protocol, kind)() == msg
    assert protocol.encode(msg).strip() == '{"v":1,"type":"%s"}' % kind
    assert roundtrip(msg) == msg


@pytest.mark.parametrize("level", ["info", "warn", "error"])
def test_log_roundtrip(level):
    msg = protocol.log(level, "model loaded")
    assert msg == {"v": 1, "type": "log", "level": level, "msg": "model loaded"}
    assert roundtrip(msg) == msg


def test_encode_is_utf8_and_single_line():
    msg = protocol.result("d1", text="café\nnaïve", speech_ms=1, infer_ms=1, segments=[])
    line = protocol.encode(msg)
    assert "café" in line, "UTF-8, not \\u escapes"
    assert line.count("\n") == 1, "embedded newlines are escaped"


@pytest.mark.parametrize(
    "line",
    [
        "not json",
        "[1,2]",
        '{"type":"ping"}',
        '{"v":2,"type":"ping"}',
        '{"v":1,"type":"teleport"}',
        '{"v":1}',
    ],
)
def test_parse_rejects_bad_lines(line):
    with pytest.raises(ProtocolError):
        protocol.parse(line)


# ------------------------------------------------------------------------------------------ handler

from wren_sidecar.engine import Handler  # noqa: E402


def write_wav(path: Path, samples: list[int], rate: int = 16000) -> Path:
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(b"".join(int(s).to_bytes(2, "little", signed=True) for s in samples))
    return path


def test_ping_and_not_ready_before_model():
    h = Handler(transcriber=None)
    assert h.handle({"v": 1, "type": "ping"}) == [{"v": 1, "type": "pong"}]
    out = h.handle(protocol.parse('{"v":1,"type":"transcribe","id":"early","audio_path":"x.wav"}'))
    assert out == [protocol.error("early", "NOT_READY", "model is still loading")]


def models_dir() -> Path:
    env = os.environ.get("WREN_MODELS_DIR")
    if env:
        return Path(env)
    return Path(os.environ.get("LOCALAPPDATA", ".")) / "Wren" / "models"


@pytest.fixture(scope="module")
def handler():
    from wren_sidecar.engine import EngineConfig, load_transcriber

    name = os.environ.get("WREN_TEST_MODEL", "large-v3-turbo")
    if not (models_dir() / name / "model.bin").is_file():
        pytest.skip(f"model {name} not found under {models_dir()}")
    cfg = EngineConfig(models_dir=models_dir(), model=name)
    transcriber = load_transcriber(cfg, log=lambda level, msg: None)
    return Handler(transcriber=transcriber)


def test_missing_file_is_bad_audio(handler, tmp_path):
    out = handler.handle(
        protocol.parse(json.dumps({"v": 1, "type": "transcribe", "id": "m1", "audio_path": str(tmp_path / "nope.wav")}))
    )
    assert len(out) == 1
    assert out[0]["type"] == "error" and out[0]["id"] == "m1" and out[0]["code"] == "BAD_AUDIO"


def test_garbage_file_is_bad_audio(handler, tmp_path):
    bad = tmp_path / "bad.wav"
    bad.write_bytes(b"this is not audio at all")
    out = handler.handle({"v": 1, "type": "transcribe", "id": "g1", "audio_path": str(bad), "bias_terms": [], "prompt": ""})
    assert out[0]["type"] == "error" and out[0]["code"] == "BAD_AUDIO"


def test_silent_wav_gives_empty_result(handler, tmp_path):
    silent = write_wav(tmp_path / "silent.wav", [0] * 16000 * 3)
    out = handler.handle({"v": 1, "type": "transcribe", "id": "s1", "audio_path": str(silent), "bias_terms": [], "prompt": ""})
    assert len(out) == 1
    res = out[0]
    assert res["type"] == "result" and res["id"] == "s1"
    assert res["text"] == ""
    assert res["speech_ms"] < 300
    assert isinstance(res["infer_ms"], int) and res["infer_ms"] >= 0


def test_empty_wav_gives_empty_result(handler, tmp_path):
    empty = write_wav(tmp_path / "empty.wav", [])
    out = handler.handle({"v": 1, "type": "transcribe", "id": "e1", "audio_path": str(empty), "bias_terms": [], "prompt": ""})
    assert out[0]["type"] == "result" and out[0]["text"] == "" and out[0]["speech_ms"] == 0


def test_speech_fixture_transcribes(handler):
    fixture = Path(__file__).resolve().parents[2] / "src-tauri" / "tests" / "fixtures" / "weather.wav"
    if not fixture.is_file():
        pytest.skip("run scripts/make-test-wavs.ps1 first")
    out = handler.handle(
        {"v": 1, "type": "transcribe", "id": "w1", "audio_path": str(fixture), "bias_terms": ["Wren"], "prompt": "Hello."}
    )
    res = out[0]
    assert res["type"] == "result", res
    assert "weather" in res["text"].lower()
    assert res["speech_ms"] > 300
    assert res["segments"] and all(set(s) == {"start", "end", "text", "no_speech_prob"} for s in res["segments"])


# ------------------------------------------------------------------------------------------ fallback + process


def test_cuda_failure_falls_back_to_cpu_model(monkeypatch, tmp_path):
    from wren_sidecar import cuda
    from wren_sidecar.engine import EngineConfig, load_transcriber

    if not (models_dir() / "small.en" / "model.bin").is_file():
        pytest.skip("small.en not installed")
    monkeypatch.setattr(cuda, "cuda_problem", lambda: "no CUDA GPU found")
    logs = []
    t = load_transcriber(EngineConfig(models_dir=models_dir()), log=lambda lvl, msg: logs.append((lvl, msg)))
    assert (t.name, t.device, t.compute) == ("small.en", "cpu", "int8")
    assert any(lvl == "warn" and "CPU" in msg and "no CUDA GPU found" in msg for lvl, msg in logs)
    silent = write_wav(tmp_path / "s.wav", [0] * 16000)
    res = Handler(t).handle({"v": 1, "type": "transcribe", "id": "c1", "audio_path": str(silent), "bias_terms": [], "prompt": ""})[0]
    assert res["type"] == "result" and res["text"] == ""


def test_process_speaks_only_protocol_on_stdout(tmp_path):
    """The real entry point: ready, ping/pong, NOT_READY never after ready, result, shutdown -> exit 0."""
    import subprocess
    import sys

    if not (models_dir() / "small.en" / "model.bin").is_file():
        pytest.skip("small.en not installed")
    silent = write_wav(tmp_path / "s.wav", [0] * 16000)
    proc = subprocess.Popen(
        [sys.executable, "-m", "wren_sidecar", "--models-dir", str(models_dir()), "--model", "small.en",
         "--compute", "int8", "--beam", "1", "--cpu-fallback", "small.en", "--device", "cpu"],
        cwd=Path(__file__).resolve().parents[1], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True, encoding="utf-8",
    )
    try:
        lines = []
        while True:
            line = proc.stdout.readline()
            assert line, "sidecar exited before ready"
            msg = protocol.parse(line)  # every stdout line must be a valid message
            lines.append(msg)
            if msg["type"] == "ready":
                break
        assert lines[-1]["device"] == "cpu" and lines[-1]["model"] == "small.en"
        proc.stdin.write(protocol.encode(protocol.ping()))
        proc.stdin.write(protocol.encode({"v": 1, "type": "transcribe", "id": "p1", "audio_path": str(silent)}))
        proc.stdin.write("garbage\n")
        proc.stdin.flush()
        got = [protocol.parse(proc.stdout.readline()) for _ in range(3)]
        kinds = sorted(m["type"] for m in got)
        assert kinds == ["error", "pong", "result"], got
        assert next(m for m in got if m["type"] == "error")["id"] is None
        assert next(m for m in got if m["type"] == "result")["text"] == ""
        proc.stdin.write(protocol.encode(protocol.shutdown()))
        proc.stdin.flush()
        assert proc.wait(timeout=20) == 0
    finally:
        if proc.poll() is None:
            proc.kill()


def test_process_exits_when_stdin_closes():
    import subprocess
    import sys

    proc = subprocess.Popen(
        [sys.executable, "-m", "wren_sidecar", "--models-dir", str(models_dir()), "--model", "small.en",
         "--compute", "int8", "--device", "cpu"],
        cwd=Path(__file__).resolve().parents[1], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
    )
    proc.stdin.close()  # Wren gone, even mid-load
    assert proc.wait(timeout=20) == 0
