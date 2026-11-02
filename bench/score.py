"""Score every result file in out/results against the Willow references and print markdown tables.

Usage: .venv/Scripts/python.exe score.py [--examples]
Writes out/tables.md (aggregate numbers only) and out/per_clip.json (per-clip WER, git-ignored).
"""
import json
import sys
sys.stdout.reconfigure(encoding="utf-8")
import re
import sys
from pathlib import Path

import jiwer
import numpy as np
from whisper_normalizer.english import EnglishTextNormalizer

from bench import VOCAB, load_dictionary_terms

OUT = Path(__file__).parent / "out"
RES = OUT / "results"
REF_CFG = "turbo-i8f16-b5"  # paired comparisons are made against this config
_norm = EnglishTextNormalizer()
FILLERS = re.compile(r"\b(erm|er|uh|um|uhm|ah|eh|hmm|mm|mhm)\b")
EQUIV = [(re.compile(r"\bokay\b"), "ok"), (re.compile(r"\balright\b"), "all right"),
         (re.compile(r"\be mail\b"), "email")]


def norm(text: str) -> str:
    # Willow pastes the replacement for a dictionary shortcut; map it back to the spoken trigger phrase.
    for t in json.loads((Path.home() / "AppData/Roaming/willow-voice/dictionary.json").read_text("utf-8"))["terms"]:
        if t.get("isReplacementEnabled") and t.get("replacement"):
            text = text.replace(t["replacement"], t["original"])
    s = _norm(text)
    s = FILLERS.sub(" ", s)
    for pat, rep in EQUIV:
        s = pat.sub(rep, s)
    return " ".join(s.split())


# Willow's clean-up drops discourse fillers and false starts that a verbatim model keeps. The lenient score
# removes these words from BOTH sides, so it measures content words rather than clean-up style.
DISCOURSE = re.compile(r"\b(like|so|just|kind of|sort of|you know|i mean|basically|actually|literally|really|"
                       r"obviously|and then|then|um|well|ok)\b")


def lenient(s: str) -> str:
    return " ".join(DISCOURSE.sub(" ", s).split())


def load(name):
    rows = [json.loads(l) for l in open(RES / f"{name}.jsonl", encoding="utf-8")]
    summ = json.loads((RES / f"{name}.summary.json").read_text("utf-8"))
    return rows, summ


def refs_for(subset):
    f = "sample.json" if subset == "sample" else "terms.json"
    return {r["id"]: r for r in json.loads((OUT / f).read_text("utf-8"))}


def wer_stats(ids, refs, hyps):
    r = [refs[i] for i in ids]
    h = [hyps[i] for i in ids]
    o = jiwer.process_words(r, h)
    n = o.hits + o.substitutions + o.deletions
    return dict(wer=(o.substitutions + o.deletions + o.insertions) / n, sub=o.substitutions / n,
                dele=o.deletions / n, ins=o.insertions / n, nwords=n)


def bootstrap_ci(ids, refs, hyps, B=1000, seed=0):
    """95% CI of corpus WER, resampling clips."""
    rng = np.random.default_rng(seed)
    errs, lens = [], []
    for i in ids:
        o = jiwer.process_words(refs[i], hyps[i]) if refs[i] else None
        errs.append(o.substitutions + o.deletions + o.insertions)
        lens.append(o.hits + o.substitutions + o.deletions)
    errs, lens = np.array(errs), np.array(lens)
    idx = rng.integers(0, len(ids), (B, len(ids)))
    w = errs[idx].sum(1) / lens[idx].sum(1)
    return np.percentile(w, 2.5), np.percentile(w, 97.5), errs, lens


def looped(text: str) -> bool:
    """A decoding loop: some 1-6 word phrase repeated 4+ times back to back."""
    w = text.split()
    for n in range(1, 7):
        for i in range(len(w) - 4 * n + 1):
            p = w[i:i + n]
            if all(w[i + k * n:i + (k + 1) * n] == p for k in range(1, 4)):
                return True
    return False


def latency(rows):
    d = np.array([r["dur"] for r in rows])
    ms = np.array([r["ms"] for r in rows])
    m = d <= 60
    b, a = np.polyfit(d[m], ms[m], 1)
    def binmed(lo, hi):
        s = ms[(d >= lo) & (d <= hi)]
        return float(np.median(s)) if len(s) else float("nan")
    return dict(median_ms=float(np.median(ms)), rtf=float(np.median(ms / 1000 / d)),
                fit5=a + 5 * b, fit10=a + 10 * b, fit15=a + 15 * b,
                b5=binmed(4, 6), b10=binmed(8, 12), b15=binmed(13, 17), p95_ms=float(np.percentile(ms, 95)),
                max_ms=float(ms.max()), total_s=float(ms.sum() / 1000))


def term_counts(text_n, terms_n):
    return {t: len(re.findall(r"\b" + re.escape(t) + r"\b", text_n)) for t in terms_n}


def main():
    names = sorted(p.name[:-6] for p in RES.glob("*.jsonl") if (RES / f"{p.name[:-6]}.summary.json").exists())
    lines = []
    sample_ref = {i: norm(r["ref"]) for i, r in refs_for("sample").items()}
    per_clip = {}
    table = []
    for name in names:
        cfg, subset = name.split("__")
        if subset != "sample":
            continue
        rows, s = load(name)
        if len(rows) < len(sample_ref):
            continue
        hyps = {r["id"]: norm(r["hyp"]) for r in rows}
        ids = [i for i in hyps if sample_ref[i]]
        st = wer_stats(ids, sample_ref, hyps)
        lo, hi, errs, lens = bootstrap_ci(ids, sample_ref, hyps)
        per_clip[cfg] = {i: (int(e), int(n)) for i, e, n in zip(ids, errs, lens)}
        lat = latency(rows)
        len_ref = {i: lenient(sample_ref[i]) for i in ids}
        len_hyp = {i: lenient(hyps[i]) for i in ids}
        lids = [i for i in ids if len_ref[i]]
        st["lenient"] = wer_stats(lids, len_ref, len_hyp)["wer"]
        dict_ids = [i for i in ids if refs_for("sample")[i]["dur"] <= 30]
        st["wer30"] = wer_stats(dict_ids, sample_ref, hyps)["wer"]
        raw = {r["id"]: r["hyp"] for r in rows}
        loops = sum(looped(hyps[i]) for i in ids)
        blowups = sum(len(hyps[i].split()) > 1.3 * len(sample_ref[i].split()) + 5 for i in ids)
        empties = sum(not hyps[i] for i in ids)
        sil = s["silence"]
        sil_bad = [k for k, v in sil.items() if norm(v["hyp"])]
        table.append(dict(cfg=cfg, **st, lo=lo, hi=hi, **lat, load=s["load_s"], first=s["first_call_ms"],
                          vram=s["vram_model_mb"], peak=s["vram_peak_mb"], fail=s["failures"], loops=loops,
                          blowups=blowups, empties=empties, sil_bad=len(sil_bad), sil_n=len(sil),
                          sil_texts=sorted({sil[k]["hyp"] for k in sil_bad})))
    table.sort(key=lambda t: t["wer"])
    lines.append("## Accuracy (sample, n=200, normalised WER vs Willow reference)\n")
    lines.append("| config | WER | 95% CI | sub | del | ins | WER ≤30 s clips | lenient WER | loops | blow-ups | empty | failures | silence clips with text |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for t in table:
        lines.append(f"| {t['cfg']} | {t['wer']*100:.2f}% | {t['lo']*100:.1f}–{t['hi']*100:.1f} | {t['sub']*100:.1f} | "
                     f"{t['dele']*100:.1f} | {t['ins']*100:.1f} | {t['wer30']*100:.2f}% | {t['lenient']*100:.2f}% | {t['loops']} | {t['blowups']} | {t['empties']} | "
                     f"{t['fail']} | {t['sil_bad']}/{t['sil_n']} |")
    lines.append("\n## Speed and memory (warm, per clip, RTX 3060 Ti)\n")
    lines.append("| config | median ms | median latency/clip-length | ~5 s clip | ~10 s clip | ~15 s clip | fit 10 s | p95 ms | max ms | cold load s | first call ms | VRAM model MB | VRAM peak total MB |")
    lines.append("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    for t in table:
        lines.append(f"| {t['cfg']} | {t['median_ms']:.0f} | {t['rtf']:.3f} | {t['b5']:.0f} | {t['b10']:.0f} | {t['b15']:.0f} | "
                     f"{t['fit10']:.0f} | {t['p95_ms']:.0f} | {t['max_ms']:.0f} | {t['load']:.1f} | {t['first']:.0f} | "
                     f"{t['vram']} | {t['peak']} |")
    lines.append("\n## Silence outputs (distinct texts emitted on no-speech clips)\n")
    for t in table:
        if t["sil_texts"]:
            lines.append(f"- {t['cfg']}: " + "; ".join(repr(x[:60]) for x in t["sil_texts"]))

    # Paired comparison: same clips, resampled together, so the shared reference noise cancels out.
    ref_cfg = REF_CFG if REF_CFG in per_clip else table[0]["cfg"]
    rng = np.random.default_rng(1)
    ids = sorted(per_clip[ref_cfg])
    idx = rng.integers(0, len(ids), (2000, len(ids)))
    def arr(cfg, which):
        return np.array([per_clip[cfg][i][which] for i in ids])
    re_, rl = arr(ref_cfg, 0), arr(ref_cfg, 1)
    base = re_[idx].sum(1) / rl[idx].sum(1)
    lines.append(f"\n## Paired WER difference vs {ref_cfg} (bootstrap over clips, 2000 resamples)\n")
    lines.append("| config | ΔWER (points) | 95% CI | P(better than ref) | clips better / same / worse |")
    lines.append("|---|---|---|---|---|")
    for t in table:
        ce = arr(t["cfg"], 0)
        d = ce[idx].sum(1) / rl[idx].sum(1) - base
        lines.append(f"| {t['cfg']} | {(ce.sum() - re_.sum()) / rl.sum() * 100:+.2f} | "
                     f"{np.percentile(d, 2.5) * 100:+.2f} to {np.percentile(d, 97.5) * 100:+.2f} | {(d < 0).mean():.2f} | "
                     f"{(ce < re_).sum()} / {(ce == re_).sum()} / {(ce > re_).sum()} |")

    # WER by length bin for every config
    sample = refs_for("sample")
    bins = [(0, 5), (5, 10), (10, 20), (20, 30), (30, 60), (60, 1e9)]
    lines.append("\n## WER by clip length\n")
    lines.append("| config | " + " | ".join(f"{a}-{b if b < 1e9 else '∞'} s" for a, b in bins) + " |")
    lines.append("|---" * (len(bins) + 1) + "|")
    for t in table:
        pc = per_clip[t["cfg"]]
        cells = []
        for a, b in bins:
            sel = [v for i, v in pc.items() if a <= sample[i]["dur"] < b]
            cells.append(f"{sum(e for e, _ in sel) / max(1, sum(n for _, n in sel)) * 100:.1f}% (n={len(sel)})")
        lines.append(f"| {t['cfg']} | " + " | ".join(cells) + " |")

    # Biasing
    lines += biasing_tables(sample_ref)
    (OUT / "tables.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    (OUT / "per_clip.json").write_text(json.dumps(per_clip), encoding="utf-8")
    print("\n".join(lines))


def biasing_tables(sample_ref):
    terms_rows = refs_for("terms")
    lines = ["\n## Biasing\n"]
    vocab_n = sorted({norm(t) for t in VOCAB + load_dictionary_terms()}, key=len, reverse=True)
    term_ref = {i: norm(r["ref"]) for i, r in terms_rows.items()}
    lines.append("| config | terms-subset WER | term recall | rest-of-sample WER (clips w/o terms) | spurious term insertions on rest |")
    lines.append("|---|---|---|---|---|")
    term_ids_in_sample = {i for i in terms_rows if i in sample_ref}
    for p in sorted(RES.glob("*__terms.jsonl")):
        if not (RES / f"{p.name[:-6]}.summary.json").exists():
            continue
        cfg = p.name.split("__")[0]
        rows, _ = load(f"{cfg}__terms")
        hyps = {r["id"]: norm(r["hyp"]) for r in rows}
        ids = [i for i in hyps if term_ref.get(i)]
        if len(ids) < len([i for i in term_ref if term_ref[i]]):
            continue  # run incomplete or in progress
        st = wer_stats(ids, term_ref, hyps)
        hit = tot = 0
        for i in ids:
            for t in {norm(x) for x in terms_rows[i]["terms"]}:
                nr = len(re.findall(r"\b" + re.escape(t) + r"\b", term_ref[i]))
                nh = len(re.findall(r"\b" + re.escape(t) + r"\b", hyps[i]))
                hit += min(nr, nh)
                tot += nr
        rest = "n/a"
        spur = "n/a"
        sp = RES / f"{cfg}__sample.jsonl"
        if sp.exists():
            srows = [json.loads(l) for l in open(sp, encoding="utf-8")]
            sh = {r["id"]: norm(r["hyp"]) for r in srows}
            rid = [i for i in sh if sample_ref[i] and i not in term_ids_in_sample]
            rest = f"{wer_stats(rid, sample_ref, sh)['wer']*100:.2f}% (n={len(rid)})"
            spur = sum(len(re.findall(r"\b" + re.escape(t) + r"\b", sh[i])) for i in rid for t in vocab_n)
        lines.append(f"| {cfg} | {st['wer']*100:.2f}% | {hit}/{tot} = {hit/max(1,tot)*100:.0f}% | {rest} | {spur} |")
    return lines


if __name__ == "__main__":
    main()
