"""Wren sidecar STT core: faster-whisper large-v3-turbo on CUDA (the benchmark's recommendation).

    stt = WrenSTT(r"C:\\Wren\\bench\\models\\large-v3-turbo", glossary=["Claude", "Claude Code", "Overseer"])
    text = stt.transcribe(audio)   # audio: 1-D float32 numpy array, 16 kHz mono, range [-1, 1]

Run `python wren_stt.py` to self-test on this machine (loads the model, checks silence gives "", times a
real clip from out/audio and synthetic 5/10/15 s inputs).
"""
import os
import sys
import time
from pathlib import Path

import numpy as np

SAMPLE_RATE = 16_000


def _add_cuda_dlls() -> None:
    """The nvidia-cublas-cu12 / nvidia-cudnn-cu12 wheels put their DLLs in site-packages/nvidia/*/bin, which
    Windows does not search. CTranslate2 loads cuBLAS/cuDNN by name, so register each folder and prepend PATH.
    (Same recipe as Overseer's voice/stt.py.)"""
    root = Path(sys.prefix) / "Lib" / "site-packages" / "nvidia"
    for bin_dir in sorted(root.glob("*/bin")):
        os.add_dll_directory(str(bin_dir))
        os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")


class WrenSTT:
    def __init__(self, model_dir, glossary=(), beam_size: int = 1, compute_type: str = "int8_float16"):
        _add_cuda_dlls()
        from faster_whisper import WhisperModel

        # device="cuda" explicitly: "auto" would drop to CPU silently if the CUDA DLLs failed to load.
        # local_files_only: never contact Hugging Face at runtime.
        self.model = WhisperModel(str(model_dir), device="cuda", compute_type=compute_type, local_files_only=True)
        self.beam_size = beam_size
        self.set_glossary(glossary)
        self.transcribe(np.zeros(SAMPLE_RATE, dtype=np.float32))  # warm-up: CUDA kernels, VAD session

    def set_glossary(self, terms) -> None:
        # Glossary goes in as the initial prompt (not `hotwords`, which made the model append dictionary words
        # to trailing silence in the benchmark). Whisper keeps only ~220 prompt tokens: keep it to ~100 terms.
        terms = [t.strip() for t in terms if t and t.strip()]
        self.prompt = ("Glossary: " + ", ".join(terms) + ".") if terms else None

    def transcribe(self, audio: np.ndarray) -> str:
        audio = np.asarray(audio, dtype=np.float32).reshape(-1)
        segments, _ = self.model.transcribe(
            audio,
            language="en",
            beam_size=self.beam_size,
            vad_filter=True,                  # silence/trailing pauses -> no "Thank you." hallucinations
            condition_on_previous_text=False,  # no carry-over loops on long dictations
            initial_prompt=self.prompt,
        )
        # `segments` is a lazy generator: decoding happens while it is consumed.
        return " ".join(s.text.strip() for s in segments).strip()


def _self_test() -> None:
    here = Path(__file__).parent
    t0 = time.perf_counter()
    stt = WrenSTT(here / "models" / "large-v3-turbo", glossary=["Claude", "Claude Code", "Overseer", "Taroting"])
    print(f"load + warm-up: {time.perf_counter() - t0:.1f} s")

    silent = stt.transcribe(np.zeros(5 * SAMPLE_RATE, dtype=np.float32))
    assert silent == "", f"silence produced text: {silent!r}"
    print("5 s of silence -> '' (ok)")

    clips = sorted((here / "out" / "audio").glob("*.wav"))
    if clips:
        import soundfile as sf
        audio, sr = sf.read(clips[0], dtype="float32")
        assert sr == SAMPLE_RATE and audio.ndim == 1
        pool = np.concatenate([sf.read(c, dtype="float32")[0] for c in clips[:20]])
        t = time.perf_counter()
        text = stt.transcribe(audio)
        print(f"real clip {len(audio) / SAMPLE_RATE:.1f} s -> {len(text.split())} words in "
              f"{(time.perf_counter() - t) * 1000:.0f} ms")
        assert text, "real speech produced no text"
        for sec in (5, 10, 15):
            x = pool[: sec * SAMPLE_RATE]
            stt.transcribe(x)
            t = time.perf_counter()
            n = len(stt.transcribe(x).split())
            print(f"{sec:2d} s of his speech -> {n} words in {(time.perf_counter() - t) * 1000:.0f} ms (warm)")
    print("self-test passed")


if __name__ == "__main__":
    _self_test()
