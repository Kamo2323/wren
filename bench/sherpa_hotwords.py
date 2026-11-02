"""Does contextual biasing (hotwords) work for Parakeet TDT 0.6B v2 through sherpa-onnx?

Runs the sherpa-onnx int8 export (CPU) over the dictionary-term clips three ways: greedy, modified beam
search without hotwords, and modified beam search with the vocabulary as hotwords. Run with the separate
venv: .venv-sherpa/Scripts/python.exe sherpa_hotwords.py [--limit N] [--score 2.0]
Writes out/results/sherpa-<mode>__terms.jsonl (+ .summary.json) so score.py picks them up.
"""
import argparse
import json
import sys
import time
from pathlib import Path

import soundfile as sf

HERE = Path(__file__).parent
M = HERE / "models" / "sherpa-parakeet-tdt-0.6b-v2-int8"
OUT = HERE / "out"
sys.path.insert(0, str(HERE))


def vocab_terms():
    # Same lists as bench.py, imported textually so this venv needs no faster-whisper.
    src = (HERE / "bench.py").read_text(encoding="utf-8")
    ns = {}
    start = src.index("VOCAB = [")
    exec(src[start:src.index("]", start) + 1], ns)
    d = json.loads((Path.home() / "AppData/Roaming/willow-voice/dictionary.json").read_text(encoding="utf-8"))
    return sorted(set(ns["VOCAB"] + [t["original"] for t in d["terms"] if not t.get("isReplacementEnabled")]))


def bpe_vocab() -> Path:
    """sherpa-onnx tokenises hotwords with a BPE vocab ('piece<TAB>score'). The export ships only tokens.txt,
    so build one: SentencePiece BPE scores are minus the merge order, approximated here by minus the id."""
    p = M / "bpe.vocab"
    lines = []
    for line in (M / "tokens.txt").read_text(encoding="utf-8").splitlines():
        piece, idx = line.rsplit(" ", 1)
        if piece not in ("<unk>", "<blk>"):
            lines.append(f"{piece}\t{-int(idx)}")
    p.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return p


def make(mode, score):
    import sherpa_onnx
    kw = dict(encoder=str(M / "encoder.int8.onnx"), decoder=str(M / "decoder.int8.onnx"),
              joiner=str(M / "joiner.int8.onnx"), tokens=str(M / "tokens.txt"), num_threads=6,
              feature_dim=128, model_type="nemo_transducer")
    if mode == "greedy":
        return sherpa_onnx.OfflineRecognizer.from_transducer(decoding_method="greedy_search", **kw)
    if mode == "mbs":
        return sherpa_onnx.OfflineRecognizer.from_transducer(decoding_method="modified_beam_search",
                                                             max_active_paths=4, **kw)
    hw = OUT / "hotwords.txt"
    hw.write_text("\n".join(vocab_terms()) + "\n", encoding="utf-8")
    return sherpa_onnx.OfflineRecognizer.from_transducer(
        decoding_method="modified_beam_search", max_active_paths=4, hotwords_file=str(hw), hotwords_score=score,
        modeling_unit="bpe", bpe_vocab=str(bpe_vocab()), **kw)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--score", type=float, default=2.0)
    ap.add_argument("--modes", default="greedy,mbs,hotwords")
    a = ap.parse_args()
    rows = json.loads((OUT / "terms.json").read_text(encoding="utf-8"))[: a.limit or None]
    for mode in a.modes.split(","):
        rec = make(mode, a.score)
        name = f"sherpa-{mode}" + (f"{a.score:g}" if mode == "hotwords" else "")
        t0 = time.perf_counter()
        with open(OUT / "results" / f"{name}__terms.jsonl", "w", encoding="utf-8") as fh:
            for r in rows:
                x, sr = sf.read(r["wav"], dtype="float32")
                s = rec.create_stream()
                s.accept_waveform(sr, x)
                t = time.perf_counter()
                rec.decode_stream(s)
                fh.write(json.dumps({"id": r["id"], "dur": r["dur"], "ms": round((time.perf_counter() - t) * 1000, 1),
                                     "hyp": s.result.text.strip(), "err": None}) + "\n")
        (OUT / "results" / f"{name}__terms.summary.json").write_text(json.dumps(
            {"config": name, "n": len(rows), "note": "sherpa-onnx int8 on CPU (6 threads); latency not comparable"}))
        print(name, f"{time.perf_counter() - t0:.0f}s", flush=True)


if __name__ == "__main__":
    main()
