"""Build the benchmark sample from the owner's Willow Voice history (read-only).

Pairing: each Transcripts/<id>.json has `audioFilePath` -> Recordings/<id>.aac (ADTS AAC, 48 kHz mono).
The reference text is the transcript's `query` field (Willow's final output).

Writes (git-ignored, contain dictated text):
  out/manifest_all.json  every usable pair with its duration
  out/sample.json        the fixed-seed random sample
  out/audio/<id>.wav     16 kHz mono PCM16 copies of the sampled clips
  out/silence/*.wav      synthetic + real room-tone clips with no speech
Never touches supabase-session.json or any other credential file.
"""
import json
import random
import subprocess
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
import soundfile as sf

WILLOW = Path.home() / "AppData/Roaming/willow-voice"
OUT = Path(__file__).parent / "out"
SEED = 20261102
N = 200
MIN_SEC = 2.0   # "very short" clips (a word or two) say little about dictation accuracy
MIN_WORDS = 3
MAX_SEC = 300.0


def probe(path: Path) -> float:
    r = subprocess.run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
                       capture_output=True, text=True)
    try:
        return float(r.stdout.strip())
    except ValueError:
        return -1.0


def decode(src: Path, dst: Path) -> None:
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(src), "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le",
                    str(dst)], check=True)


def main():
    OUT.mkdir(exist_ok=True)
    rows, skipped = [], {}
    def skip(reason):
        skipped[reason] = skipped.get(reason, 0) + 1
    for f in sorted((WILLOW / "Transcripts").glob("*.json")):
        d = json.loads(f.read_text(encoding="utf-8"))
        audio = Path(d.get("audioFilePath") or "")
        if d.get("status") != "completed":
            skip("status not completed"); continue
        if d.get("llmType") != "normal":
            skip(f"llmType={d.get('llmType')}"); continue   # email mode / scribe = AI rewrites, not dictation
        if not audio.is_file() or audio.stat().st_size == 0:
            skip("audio missing/empty"); continue
        text = (d.get("query") or "").strip()
        if len(text.split()) < MIN_WORDS:
            skip("reference < 3 words"); continue
        rows.append({"id": d["id"], "audio": str(audio), "ref": text})
    with ThreadPoolExecutor(8) as ex:
        for r, dur in zip(rows, ex.map(lambda r: probe(Path(r["audio"])), rows)):
            r["dur"] = round(dur, 3)
    usable = []
    for r in rows:
        if r["dur"] < MIN_SEC:
            skip(f"audio < {MIN_SEC}s"); continue
        if r["dur"] > MAX_SEC:
            skip(f"audio > {MAX_SEC}s"); continue
        usable.append(r)
    (OUT / "manifest_all.json").write_text(json.dumps(usable, indent=1), encoding="utf-8")
    sample = random.Random(SEED).sample(usable, N)
    (OUT / "audio").mkdir(exist_ok=True)
    with ThreadPoolExecutor(8) as ex:
        list(ex.map(lambda r: decode(Path(r["audio"]), OUT / "audio" / f"{r['id']}.wav"), sample))
    for r in sample:
        r["wav"] = str(OUT / "audio" / f"{r['id']}.wav")
    (OUT / "sample.json").write_text(json.dumps(sample, indent=1), encoding="utf-8")
    make_silence(sample)
    print("usable pairs:", len(usable), "skipped:", skipped)
    for name, rs in (("all usable", usable), ("sample", sample)):
        d = np.array([r["dur"] for r in rs])
        print(f"{name:10s} n={len(d)} total={d.sum()/60:.1f} min  min={d.min():.1f}s p10={np.percentile(d,10):.1f} "
              f"p25={np.percentile(d,25):.1f} median={np.median(d):.1f} p75={np.percentile(d,75):.1f} "
              f"p90={np.percentile(d,90):.1f} max={d.max():.1f}s")
    d = np.array([r["dur"] for r in sample])
    bins = [0, 5, 10, 20, 30, 60, 1e9]
    print("sample bins:", {f"{a}-{b if b < 1e9 else 'inf'}s": int(((d >= a) & (d < b)).sum()) for a, b in zip(bins, bins[1:])})


def make_silence(sample):
    """No-speech clips: digital zero, faint white noise, and the owner's own room tone
    (the quietest 30 ms frames cut from his real recordings, so mic hiss and room noise are real)."""
    sd = OUT / "silence"
    sd.mkdir(exist_ok=True)
    rng = np.random.default_rng(SEED)
    sr = 16000
    for sec in (5, 15):
        sf.write(sd / f"zeros_{sec}s.wav", np.zeros(sr * sec, np.float32), sr)
        sf.write(sd / f"noise-60dB_{sec}s.wav", (rng.standard_normal(sr * sec) * 10 ** (-60 / 20)).astype(np.float32), sr)
    frames = []
    for r in sample[:80]:
        a, _ = sf.read(r["wav"], dtype="float32")
        fl = 480
        fr = a[: len(a) // fl * fl].reshape(-1, fl)
        rms = np.sqrt((fr ** 2).mean(1))
        quiet = fr[rms <= np.percentile(rms, 8)]   # bottom 8% energy frames of each clip
        frames.append(quiet)
    tone = np.concatenate(frames).reshape(-1)
    for sec in (5, 15):
        for k in range(3):
            start = int(rng.integers(0, len(tone) - sr * sec))
            sf.write(sd / f"roomtone_{sec}s_{k}.wav", tone[start:start + sr * sec], sr)



def build_terms():
    """Biasing subset: every usable clip (whole history, not just the sample) whose reference contains a
    dictionary or vocabulary term. Written to out/terms.json, audio decoded alongside the sample's."""
    import re
    from bench import VOCAB, load_dictionary_terms
    terms = sorted(set(VOCAB + load_dictionary_terms()), key=len, reverse=True)
    usable = json.loads((OUT / "manifest_all.json").read_text(encoding="utf-8"))
    hits = []
    for r in usable:
        found = [t for t in terms if re.search(r"\b" + re.escape(t) + r"\b", r["ref"], re.I)]
        if found:
            hits.append(dict(r, terms=found, wav=str(OUT / "audio" / f"{r['id']}.wav")))
    with ThreadPoolExecutor(8) as ex:
        list(ex.map(lambda r: Path(r["wav"]).exists() or decode(Path(r["audio"]), Path(r["wav"])), hits))
    (OUT / "terms.json").write_text(json.dumps(hits, indent=1), encoding="utf-8")
    from collections import Counter
    c = Counter(t for r in hits for t in r["terms"])
    d = np.array([r["dur"] for r in hits])
    print(f"term clips: {len(hits)}, {d.sum()/60:.1f} min, median {np.median(d):.1f}s; term counts: {dict(c)}")


if __name__ == "__main__":
    main()
    build_terms()
