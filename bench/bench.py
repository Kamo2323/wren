"""Run one model configuration over a clip subset and record per-clip outputs, latency and VRAM.

Usage:
  .venv/Scripts/python.exe bench.py CONFIG [--subset sample|terms] [--limit N]
  .venv/Scripts/python.exe bench.py --list

Each run is its own process so cold load and VRAM are measured from a clean CUDA context.
Writes out/results/<CONFIG>__<subset>.jsonl (per clip) and .summary.json (git-ignored).
"""
import argparse
import json
import os
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

import numpy as np
import soundfile as sf

HERE = Path(__file__).parent
OUT = HERE / "out"
MODELS = HERE / "models"


def add_cuda_dlls() -> None:
    """Same idea as Overseer's stt.py: the nvidia-*-cu12 wheels put DLLs in site-packages/nvidia/*/bin,
    which Windows does not search. Register every such folder and prepend it to PATH."""
    root = Path(sys.prefix) / "Lib" / "site-packages" / "nvidia"
    for bin_dir in sorted(root.glob("*/bin")):
        os.add_dll_directory(str(bin_dir))
        os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")


def load_dictionary_terms() -> list[str]:
    """Vocabulary words from the owner's Willow dictionary. Entries with a replacement are text-expansion
    shortcuts (spoken trigger -> pasted text), not vocabulary, so only plain entries are used for biasing."""
    d = json.loads((Path.home() / "AppData/Roaming/willow-voice/dictionary.json").read_text(encoding="utf-8"))
    return [t["original"] for t in d["terms"] if not t.get("isReplacementEnabled")]


# A realistic Wren dictionary, written from what the owner works on (his projects and tools), NOT mined from
# the reference transcripts: some of these never occur in the sample, as in real life.
VOCAB = ["Claude", "Claude Code", "Overseer", "Taroting", "Cloudflare", "GitHub", "Whop", "ElevenLabs",
         "Automotosol", "XccelerAIte", "Honcho", "Maximilian", "Wren", "Willow Voice", "Codex", "Opus", "Sonnet",
         "Haiku", "Jarvis", "Discord", "Unity", "Twilio", "Resend", "Supabase", "Tauri", "Electron", "Parakeet"]


def prompt_text(terms: list[str]) -> str:
    return "Glossary: " + ", ".join(terms) + "."


# ----------------------------------------------------------------------------------------------- engines
class FasterWhisper:
    def __init__(self, model, compute_type="int8_float16", beam=5, vad=False, timestamps=True,
                 bias=None, bias_terms="dict", batched=0):
        add_cuda_dlls()
        from faster_whisper import BatchedInferencePipeline, WhisperModel
        self.model = WhisperModel(str(MODELS / model), device="cuda", compute_type=compute_type,
                                  local_files_only=True)
        self.pipe = BatchedInferencePipeline(self.model) if batched else None
        self.batched = batched
        terms = load_dictionary_terms() if bias_terms == "dict" else VOCAB
        self.kw = dict(language="en", beam_size=beam, vad_filter=vad, condition_on_previous_text=False,
                       without_timestamps=not timestamps)
        if bias == "prompt":
            self.kw["initial_prompt"] = prompt_text(terms)
        elif bias == "hotwords":
            self.kw["hotwords"] = " ".join(terms)

    def __call__(self, audio: np.ndarray) -> str:
        if self.pipe:
            kw = {k: v for k, v in self.kw.items() if k not in ("condition_on_previous_text", "vad_filter")}
            segments, _ = self.pipe.transcribe(audio, batch_size=self.batched, **kw)
        else:
            segments, _ = self.model.transcribe(audio, **self.kw)
        return " ".join(s.text.strip() for s in segments).strip()   # generator: decoding happens here


class Parakeet:
    def __init__(self, model="parakeet-tdt-0.6b-v2", quantization=None, algo="HEURISTIC", arena="kNextPowerOfTwo",
                 vad_long=0):
        add_cuda_dlls()
        import onnx_asr
        import onnxruntime as ort
        ort.preload_dlls(directory="")
        # kSameAsRequested stops the CUDA arena doubling its reservation on every growth step.
        cuda = ("CUDAExecutionProvider", {"cudnn_conv_algo_search": algo, "arena_extend_strategy": arena})
        self.model = onnx_asr.load_model("nemo-parakeet-tdt-0.6b-v2", str(MODELS / model),
                                         quantization=quantization, providers=[cuda, "CPUExecutionProvider"])
        # vad_long: clips longer than this many seconds are cut by Silero VAD into <= vad_long s pieces at
        # speech gaps (huge min_silence => merge everything until the length cap), bounding encoder memory.
        self.vad_long = vad_long
        if vad_long:
            vad = onnx_asr.load_vad("silero", str(MODELS / "silero-vad"))
            self.long = self.model.with_vad(vad, batch_size=1, max_speech_duration_s=vad_long,
                                            min_silence_duration_ms=60_000, speech_pad_ms=200)
        enc = self.model.asr._encoder if hasattr(self.model, "asr") else None
        self.providers = enc.get_providers() if enc is not None else None
        if not self.providers or self.providers[0] != "CUDAExecutionProvider":
            raise RuntimeError(f"Parakeet encoder is not on CUDA (providers={self.providers}); refusing a CPU run")

    def __call__(self, audio: np.ndarray) -> str:
        if self.vad_long and len(audio) > self.vad_long * 16000:
            return " ".join(seg.text.strip() for seg in self.long.recognize(audio, sample_rate=16000)).strip()
        return self.model.recognize(audio, sample_rate=16000).strip()


W = "large-v3-turbo"
D = "distil-large-v3.5"
CONFIGS = {
    # model comparison (no biasing, no VAD)
    "turbo-f16-b5":      (FasterWhisper, dict(model=W, compute_type="float16", beam=5)),
    "turbo-i8f16-b5":    (FasterWhisper, dict(model=W, compute_type="int8_float16", beam=5)),
    "distil-f16-b5":     (FasterWhisper, dict(model=D, compute_type="float16", beam=5)),
    "distil-i8f16-b5":   (FasterWhisper, dict(model=D, compute_type="int8_float16", beam=5)),
    "smallen-i8f16-b5":  (FasterWhisper, dict(model="small.en", compute_type="int8_float16", beam=5)),
    "parakeet-fp32":     (Parakeet, dict()),
    "parakeet-int8":     (Parakeet, dict(quantization="int8")),
    "parakeet-fp32-samereq": (Parakeet, dict(arena="kSameAsRequested")),
    "parakeet-fp32-vad30": (Parakeet, dict(arena="kSameAsRequested", vad_long=30)),
    # settings sweeps on the whisper models
    "turbo-i8f16-b1":    (FasterWhisper, dict(model=W, compute_type="int8_float16", beam=1)),
    "turbo-i8f16-b5-vad": (FasterWhisper, dict(model=W, compute_type="int8_float16", beam=5, vad=True)),
    "turbo-i8f16-b5-nots": (FasterWhisper, dict(model=W, compute_type="int8_float16", beam=5, timestamps=False)),
    "turbo-i8f16-batch8": (FasterWhisper, dict(model=W, compute_type="int8_float16", beam=5, batched=8)),
    "distil-f16-b1":     (FasterWhisper, dict(model=D, compute_type="float16", beam=1)),
    "distil-f16-b5-vad": (FasterWhisper, dict(model=D, compute_type="float16", beam=5, vad=True)),
}
# the combination the single-factor results point to: VAD on + glossary prompt
for beam in (1, 5):
    CONFIGS[f"turbo-i8f16-b{beam}-vad-prompt-vocab"] = (FasterWhisper, dict(
        model=W, compute_type="int8_float16", beam=beam, vad=True, bias="prompt", bias_terms="vocab"))
for base, m in (("turbo-i8f16-b5", W), ("distil-f16-b5", D)):
    ct = CONFIGS[base][1]["compute_type"]
    for terms in ("dict", "vocab"):
        for bias in ("prompt", "hotwords"):
            CONFIGS[f"{base}-{bias}-{terms}"] = (FasterWhisper, dict(model=m, compute_type=ct, beam=5,
                                                                      bias=bias, bias_terms=terms))


# ----------------------------------------------------------------------------------------------- VRAM
class VramMonitor:
    """Polls total GPU memory.used. Per-process numbers are N/A under Windows WDDM, so the model's
    footprint is reported as peak during the run minus the baseline taken just before the model loads."""

    def __init__(self):
        self.samples = []
        self.proc = subprocess.Popen(["nvidia-smi", "--query-gpu=memory.used", "--format=csv,noheader,nounits",
                                      "-lms", "100"], stdout=subprocess.PIPE, text=True)
        self.t = threading.Thread(target=self._read, daemon=True)
        self.t.start()

    def _read(self):
        for line in self.proc.stdout:
            try:
                self.samples.append((time.perf_counter(), int(line.strip())))
            except ValueError:
                pass

    def window(self, t0, t1):
        return [v for t, v in self.samples if t0 <= t <= t1]

    def stop(self):
        self.proc.terminate()


def subset_rows(subset: str):
    if subset == "sample":
        return json.loads((OUT / "sample.json").read_text(encoding="utf-8"))
    return json.loads((OUT / "terms.json").read_text(encoding="utf-8"))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("config", nargs="?")
    ap.add_argument("--subset", default="sample")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--list", action="store_true")
    a = ap.parse_args()
    if a.list:
        print("\n".join(CONFIGS)); return
    cls, kw = CONFIGS[a.config]
    rows = subset_rows(a.subset)[: a.limit or None]
    audio = {r["id"]: sf.read(r["wav"], dtype="float32")[0] for r in rows}
    silence = {p.stem: sf.read(p, dtype="float32")[0] for p in sorted((OUT / "silence").glob("*.wav"))}

    mon = VramMonitor()
    time.sleep(1.5)
    t_base = time.perf_counter()
    base = mon.window(t_base - 1.2, t_base)
    baseline = statistics.median(base) if base else None

    t0 = time.perf_counter()
    eng = cls(**kw)
    load_s = time.perf_counter() - t0
    # first call on a ~5 s clip = cold inference (CUDA kernels, cuDNN plans, allocator growth)
    first = min(rows, key=lambda r: abs(r["dur"] - 5.0))
    t1 = time.perf_counter()
    eng(audio[first["id"]])
    first_ms = (time.perf_counter() - t1) * 1000
    for _ in range(2):
        eng(audio[first["id"]])

    res_dir = OUT / "results"
    res_dir.mkdir(exist_ok=True)
    name = f"{a.config}__{a.subset}"
    failures = 0
    with open(res_dir / f"{name}.jsonl", "w", encoding="utf-8") as fh:
        for i, r in enumerate(rows):
            t = time.perf_counter()
            try:
                hyp, err = eng(audio[r["id"]]), None
            except Exception as e:  # recorded, not fatal: failures are a result
                hyp, err = "", f"{type(e).__name__}: {e}"[:300]
                failures += 1
            ms = (time.perf_counter() - t) * 1000
            fh.write(json.dumps({"id": r["id"], "dur": r["dur"], "ms": round(ms, 1), "hyp": hyp, "err": err}) + "\n")
            if (i + 1) % 25 == 0:
                print(f"  {i + 1}/{len(rows)}", flush=True)
        sil = {}
        for k, x in silence.items():
            t = time.perf_counter()
            try:
                out = eng(x)
            except Exception as e:
                out = f"<error {type(e).__name__}>"
            sil[k] = {"hyp": out, "ms": round((time.perf_counter() - t) * 1000, 1)}
    t_end = time.perf_counter()
    time.sleep(0.3)
    peak = max(mon.window(t_base, t_end + 0.3) or [0])
    mon.stop()
    summary = {
        "config": a.config, "subset": a.subset, "kwargs": kw, "n": len(rows), "failures": failures,
        "load_s": round(load_s, 2), "first_call_ms": round(first_ms, 1), "first_call_dur": first["dur"],
        "vram_baseline_mb": baseline, "vram_peak_mb": peak,
        "vram_model_mb": (peak - baseline) if baseline is not None else None,
        "silence": sil, "providers": getattr(eng, "providers", None),
    }
    (res_dir / f"{name}.summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    print(json.dumps({k: v for k, v in summary.items() if k not in ("silence", "kwargs")}))


if __name__ == "__main__":
    main()
