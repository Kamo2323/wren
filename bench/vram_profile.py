"""VRAM and latency as a function of dictation length, for one configuration.

Usage: .venv/Scripts/python.exe vram_profile.py CONFIG
Builds exact-length inputs (5..240 s) by concatenating the owner's sampled clips, runs each twice and
records the peak total-GPU memory above the pre-load baseline, plus the second (warm) call's latency.
Writes out/results/<CONFIG>.vram.json.
"""
import json
import statistics
import sys
import time

import numpy as np
import soundfile as sf

from bench import CONFIGS, OUT, VramMonitor

LENGTHS = [5, 10, 20, 30, 60, 120, 240]


def main(cfg):
    cls, kw = CONFIGS[cfg]
    sample = json.loads((OUT / "sample.json").read_text(encoding="utf-8"))
    pool = np.concatenate([sf.read(r["wav"], dtype="float32")[0] for r in sample[:60]])
    mon = VramMonitor()
    time.sleep(1.5)
    t = time.perf_counter()
    base = statistics.median(mon.window(t - 1.2, t))
    eng = cls(**kw)
    time.sleep(1.0)
    t = time.perf_counter()
    after_load = statistics.median(mon.window(t - 0.6, t)) - base
    rows = []
    for sec in LENGTHS:
        x = pool[: sec * 16000]
        t0 = time.perf_counter()
        eng(x)
        t1 = time.perf_counter()
        eng(x)
        t2 = time.perf_counter()
        time.sleep(0.4)
        peak = max(mon.window(t0, time.perf_counter())) - base
        rows.append({"sec": sec, "warm_ms": round((t2 - t1) * 1000), "peak_mb_above_baseline": peak})
        print(rows[-1], flush=True)
    mon.stop()
    out = {"config": cfg, "baseline_mb": base, "after_load_mb": after_load, "by_length": rows}
    (OUT / "results" / f"{cfg}.vram.json").write_text(json.dumps(out, indent=1), encoding="utf-8")
    print(json.dumps({"config": cfg, "after_load_mb": after_load}))


if __name__ == "__main__":
    main(sys.argv[1])
