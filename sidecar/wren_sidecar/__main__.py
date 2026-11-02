"""Entry point: python -m wren_sidecar --models-dir <dir> --model <name> --compute <type> --beam <n>
--cpu-fallback <name> --warmup-wav <path> [--language en] [--no-vad] [--condition-on-previous-text]
[--device auto|cpu]

stdin: requests, stdout: protocol messages only, stderr: diagnostics (never dictated text).
Order: load model -> warm-up -> `ready`. Requests that arrive before `ready` get NOT_READY; ping is
answered at any time; `shutdown` or stdin closing (Wren gone) ends the process.
"""
from __future__ import annotations

import argparse
import os
import queue
import sys
import threading
import traceback
from pathlib import Path

from . import protocol


class Out:
    """The protocol stream. Fd 1 is then pointed at stderr, so a stray print() or a native library
    writing to stdout can never corrupt the protocol."""

    def __init__(self) -> None:
        self._f = os.fdopen(os.dup(1), "w", encoding="utf-8", newline="\n", buffering=1)
        os.dup2(2, 1)
        sys.stdout = sys.stderr
        self._lock = threading.Lock()

    def send(self, msg: dict) -> None:
        line = protocol.encode(msg)
        with self._lock:
            self._f.write(line)
            self._f.flush()


def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="wren_sidecar")
    p.add_argument("--models-dir", required=True, type=Path)
    p.add_argument("--model", default="large-v3-turbo")
    p.add_argument("--compute", default="int8_float16")
    p.add_argument("--beam", default=1, type=int)
    p.add_argument("--cpu-fallback", default="small.en")
    p.add_argument("--warmup-wav", type=Path)
    p.add_argument("--language", default="en")
    p.add_argument("--no-vad", action="store_true")
    p.add_argument("--condition-on-previous-text", action="store_true")
    p.add_argument("--device", choices=["auto", "cpu"], default="auto")
    return p.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    out = Out()

    def log(level: str, msg: str) -> None:
        out.send(protocol.log(level, msg))
        print(f"[{level}] {msg}", file=sys.stderr, flush=True)

    # Imported after Out() so anything they print at import time goes to stderr.
    from .engine import EngineConfig, Handler, load_transcriber

    handler = Handler(transcriber=None)
    work: queue.Queue[dict | None] = queue.Queue()

    def reader() -> None:
        stdin = sys.stdin.buffer
        for raw in stdin:
            if not raw.strip():
                continue
            try:
                msg = protocol.parse(raw)
            except protocol.ProtocolError as e:
                out.send(protocol.error(None, "INTERNAL", f"bad request: {e}"))
                continue
            kind = msg["type"]
            if kind == "shutdown":
                work.put(None)
                if handler.transcriber is None:
                    os._exit(0)  # still loading: nothing to finish
                return
            if kind == "transcribe" and handler.transcriber is not None:
                work.put(msg)
                continue
            for reply in handler.handle(msg):  # ping, or transcribe before ready
                out.send(reply)
        # stdin closed: Wren has gone. Leave at once so no GPU memory is held.
        os._exit(0)

    threading.Thread(target=reader, name="stdin", daemon=True).start()

    cfg = EngineConfig(
        models_dir=args.models_dir,
        model=args.model,
        compute=args.compute,
        beam=args.beam,
        language=args.language,
        vad=not args.no_vad,
        condition_on_previous_text=args.condition_on_previous_text,
        cpu_fallback=args.cpu_fallback,
        device=args.device,
    )
    try:
        transcriber = load_transcriber(cfg, log, args.warmup_wav)
    except Exception as e:  # noqa: BLE001
        log("error", f"no model could be loaded ({type(e).__name__})")
        traceback.print_exc(file=sys.stderr)
        return 3
    handler.transcriber = transcriber
    out.send(
        protocol.ready(
            model=transcriber.name, device=transcriber.device, compute=transcriber.compute, vram_mb=transcriber.vram_mb
        )
    )

    while True:
        msg = work.get()
        if msg is None:
            return 0
        for reply in handler.handle(msg):
            out.send(reply)


if __name__ == "__main__":
    sys.exit(main())
