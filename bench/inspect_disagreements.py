"""Print the clips where models disagree most, for judging by eye which side is right.

Usage: .venv/Scripts/python.exe inspect_disagreements.py CONFIG_A CONFIG_B [--top 15] [--word-diffs]
Output is the owner's dictated text: read it in the terminal, do not paste it anywhere outside this PC.
"""
import argparse
import json
import sys
sys.stdout.reconfigure(encoding="utf-8")
from collections import Counter
from pathlib import Path

import jiwer

from score import OUT, RES, norm, refs_for


def load(cfg, subset="sample"):
    return {json.loads(l)["id"]: json.loads(l)["hyp"] for l in open(RES / f"{cfg}__{subset}.jsonl", encoding="utf-8")}


def clip_wer(r, h):
    if not r:
        return 0.0
    return jiwer.wer(r, h)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("a")
    ap.add_argument("b")
    ap.add_argument("--top", type=int, default=15)
    ap.add_argument("--subset", default="sample")
    ap.add_argument("--word-diffs", action="store_true", help="most common substitution pairs per config")
    x = ap.parse_args()
    refs = refs_for(x.subset)
    A, B = load(x.a, x.subset), load(x.b, x.subset)
    rows = []
    for i, r in refs.items():
        rn, an, bn = norm(r["ref"]), norm(A[i]), norm(B[i])
        rows.append((clip_wer(rn, an) - clip_wer(rn, bn), i, rn, an, bn, r["dur"]))
    rows.sort(key=lambda t: abs(t[0]), reverse=True)
    for d, i, rn, an, bn, dur in rows[: x.top]:
        print(f"\n--- {i[:8]} {dur:.1f}s  WER(A)-WER(B)={d:+.2f}")
        print(f"  REF: {rn}")
        print(f"  A  : {an}")
        print(f"  B  : {bn}")
    if x.word_diffs:
        for name, H in ((x.a, A), (x.b, B)):
            c = Counter()
            for i, r in refs.items():
                rn, hn = norm(r["ref"]), norm(H[i])
                if not rn:
                    continue
                o = jiwer.process_words(rn, hn)
                rw, hw = rn.split(), hn.split()
                for al in o.alignments[0]:
                    if al.type == "substitute":
                        c[(" ".join(rw[al.ref_start_idx:al.ref_end_idx]), " ".join(hw[al.hyp_start_idx:al.hyp_end_idx]))] += 1
            print(f"\n== {name}: top substitutions (ref -> hyp)")
            for (r, h), n in c.most_common(40):
                print(f"  {n:3d}  {r!r} -> {h!r}")


if __name__ == "__main__":
    main()
