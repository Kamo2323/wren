# Wren speech-to-text benchmark (2 Nov 2026)

## Recommendation

**faster-whisper `large-v3-turbo`, `compute_type="int8_float16"`, `beam_size=1`, `vad_filter=True`
(default Silero settings), `condition_on_previous_text=False`, the Wren dictionary passed as
`initial_prompt="Glossary: term, term, …."`, no `hotwords`.**

- **Expected latency for a 10 s dictation:** about **270–350 ms** warm, from key-up to text, on the RTX 3060 Ti
  (267 ms in the head-to-head profile, 343 ms median over real 8–12 s clips, 355 ms in the snippet self-test).
  5 s: about 210–300 ms. 15 s: about 400–600 ms.
- **VRAM:** about **1.35 GB**, flat from 5 s to 240 s inputs. Well inside the ~4 GB budget.
- **Accuracy:** the best of everything tested (9.88% WER against Willow, statistically tied with the other
  turbo settings), with the dictionary terms jumping from 80% to 93% correct.
- **Silence:** 0 of 10 no-speech clips produce text. Without VAD, every Whisper variant produced
  "Thank you." or similar on 10 of 10.
- **Cold start:** about 5–6 s to load and warm up (model files already in the OS cache). Load the model once,
  when the sidecar starts, not per dictation.

**Runner-up: Parakeet TDT 0.6B v2.** It ties on accuracy (10.11%) and is 3–6× faster (about 56–170 ms for
10 s). It loses on three counts:

- **VRAM:** the fp32 ONNX needs 3.5 GB up to 30 s and 4.7 GB at 60 s or more, which is over budget. The int8
  build fits but is 4× slower on CUDA. An fp16 conversion attempt failed (details below).
- **No biasing** through onnx-asr. Biasing works only through sherpa-onnx, and its Windows pip wheel runs on
  CPU.
- **Proper nouns:** it writes "Claude" as "clawed" 3 times in the sample, and "horoscope" as "horror scope".

It is worth revisiting if Wren ever needs streaming partials or less than 100 ms latency.

## Results

200 clips, fixed seed. WER is normalised, measured against Willow's text, and counted at corpus level.
Latency is warm, per clip. The ±0.4-point differences between the top rows are within noise. The paired
column is the one to read.

| config | WER (95% CI) | Δ vs turbo-i8f16-b5, paired (95% CI) | WER ≤30 s clips | no-speech clips with text | ~10 s clip ms | VRAM |
|---|---|---|---|---|---|---|
| **turbo int8_f16, beam 1, VAD, glossary prompt** | **9.88% (8.2–11.9)** | −0.22 (−0.61…+0.13) | 5.58% | **0/10** | 343 / 267¹ | 1.35 GB¹ |
| turbo int8_f16 beam 1 | 9.93% | −0.17 (−0.43…+0.06) | 5.25% | 10/10 | 346 | 1.4 GB |
| turbo int8_f16 beam 5 + glossary prompt | 9.93% | −0.17 (−0.43…+0.07) | 5.43% | 10/10 | 500² | 1.4 GB |
| turbo float16 beam 5 | 10.07% | −0.03 | 5.40% | 10/10 | 375 | 2.6 GB |
| turbo int8_f16 beam 5 (reference) | 10.10% (8.4–12.1) | 0 | 5.63% | 10/10 | 345 / 330¹ | 1.4 GB |
| turbo int8_f16 beam 5, VAD | 10.32% | +0.22 | 5.48% | 0/10 | 366 | 1.4 GB |
| Parakeet TDT v2 fp32 (onnx-asr, CUDA) | 10.11% (8.3–12.1) | +0.01 (−0.31…+0.35) | 6.00% | 2/10³ | 169 / 56¹ | 3.5–4.7 GB¹ |
| Parakeet TDT v2 int8 (CUDA) | 10.05% | −0.05 | 6.10% | 1/10³ | 692 | 3.6 GB |
| Parakeet fp32 + VAD split of clips over 30 s | 10.49% | +0.39 (+0.07…+0.72) | 6.00% | 2/10 | 188 | ≤3.7 GB¹ |
| distil-large-v3.5 float16 beam 5 | 10.52% | +0.43 (−0.02…+0.79) | 5.55% | 10/10 | 336 | 2.3 GB |
| distil-large-v3.5 int8_f16 beam 5 | 10.55% | +0.45 (+0.01…+0.89) | 5.48% | 10/10 | 311 / 292¹ | 1.3 GB¹ |
| small.en int8_f16 beam 5 | 10.61% | +0.51 (+0.10…+0.91) | 6.18% | 3/10 | 420 | ~0.5 GB⁴ |
| turbo `without_timestamps=True` | 11.21% | +1.12 (+0.34…+1.80) | 5.38% | 10/10 | 298 | 1.4 GB |
| turbo BatchedInferencePipeline (batch 8) | 11.80% | +1.70 (+0.79…+2.62) | 5.38% | 0/10 | 364 | 2.4 GB |

¹ From the head-to-head length profile (`vram_profile.py`). All finalists ran back to back under the same GPU
load, with exact 5–240 s inputs built from his speech. The VRAM figure is the peak above baseline.
Profile latencies in ms:

| config | 5 s | 10 s | 20 s | 30 s | 60 s | 120 s |
|---|---|---|---|---|---|---|
| turbo beam 1 + VAD + prompt | 211 | 267 | 309 | 575 | 1090 | 2100 |
| turbo beam 5 + VAD + prompt | 274 | 330 | 364 | 665 | 1324 | 2489 |
| distil int8 | 228 | 292 | 339 | 454 | 905 | 2587 |
| Parakeet fp32 | 44 | 56 | 111 | 163 | 372 | 720 |

Parakeet fp32 took 20.2 s on the 240 s input.

² Those runs overlapped a CPU-heavy sherpa-onnx job, so their latency is inflated. Their WER is unaffected.

³ Parakeet's few hits were all on "room tone" clips. Those are spliced from the quietest frames of his real
recordings and may hold faint speech, so they are not necessarily hallucinations.

⁴ The 3.8 GB in the per-run log was another app's VRAM moving. small.en weighs 0.46 GB.

Other things worth knowing:

- **Speed:** small.en is not faster than turbo on the GPU (12 decoder layers against turbo's 4), so it has no
  role here. Beam 5 buys no accuracy over beam 1 on his voice.
- **Hallucination checks:** none of the configurations produced decoding loops (a phrase repeated 4+ times),
  empty outputs or crashes. The 2–3 "blow-ups" (output much longer than the reference) per configuration are
  all long rambles where Willow's clean-up cut heavily.
- **Without VAD, Whisper also hallucinates at the end of real clips.** In trailing silence it appends
  "thank you" or "we will be right back". This is the main reason to keep `vad_filter=True`.

## Dictionary biasing

- **His Willow dictionary is tiny.** `dictionary.json` holds 4 entries: two text-expansion shortcuts ("my
  email shortcut", "concise prompt shortcut", which are replacements rather than vocabulary), "Max" and
  "Willow Voice". Only 8 of 997 usable clips contain them.
- **A realistic Wren dictionary was therefore tested as well.** It has 27 project and tool names written from
  what he works on (Claude, Claude Code, Overseer, Taroting, ElevenLabs, Twilio, Cloudflare, …), not mined from
  the references.
- **Test set:** every clip in his history whose reference contains a term: 149 clips and 268 term occurrences.

| config | term recall | WER on term clips | WER on other sample clips |
|---|---|---|---|
| turbo, no bias | 80% | 10.83% | 8.87% |
| turbo + `initial_prompt` glossary | 91% | 10.45% | 8.69% |
| turbo + `hotwords` glossary | 95% | 10.86% | 9.04% |
| **turbo beam 1 + VAD + prompt (recommended)** | **93%** | **10.32%** | **8.65%** |
| turbo + prompt / hotwords, Willow's 2-word dictionary only | 81% / 83% | 10.69% / 10.62% | 8.83% / 8.87% |
| distil, no bias / + prompt glossary | 84% / 84% | 11.44% / 11.67% | 8.98% / 9.07% |
| Parakeet fp32 (onnx-asr: no biasing option) | 79% | 10.98% | 8.80% |
| Parakeet int8 via sherpa-onnx, greedy / beam / beam + hotwords (score 2.0), CPU | 77% / 74% / 93% | 11.10% / 11.20% / 11.80% | n/a |

What the biasing results mean:

- **The prompt helps turbo where it matters and costs nothing elsewhere.** "ElevenLabs" goes from 0/6 to 6/6,
  "Twilio" from 0/4 to 4/4, and "Claude Code" from 9/17 to 14/17.
- **The few "spurious" glossary words it adds to other clips are corrections, not errors.** They are spots
  where Willow's own text was wrong (its misspellings of Taroting and ElevenLabs).
- **`hotwords` is riskier.** With his Willow dictionary, it made turbo append "Willow Voice" to the trailing
  silence of 3 unrelated clips.
- **Distil does not benefit from the prompt.**
- **Parakeet needs sherpa-onnx for biasing.** sherpa-onnx hotwords lift Parakeet's term recall (77% to 93%) but
  raise its WER. They need a BPE vocab that the export does not ship (`sherpa_hotwords.py` builds an
  approximate one), and they ran CPU-only here. With onnx-asr, Wren would have to rely on post-correction.
- **Keep the glossary short.** Whisper keeps only about 220 prompt tokens, so hold it to about 100 terms. Beyond
  that, use post-correction (a find-and-replace on the output).

## What the disagreements look like (read by eye, not listened to)

- **Insertions are 7.5 of the ~10 WER points, and they are mostly Willow's clean-up**, not model errors. Willow
  deletes "like", "so", "kind of", false starts and repeats. A "lenient" WER that drops those words on both
  sides falls to about 7.6%. Every model is verbatim in the same way, so the ranking holds.
- **British accent:** the clearest case is th-fronting. When he says "three", Willow writes "free" (for
  example "free things to settle"). turbo, distil and Parakeet all write "3", which is the correct reading.
  Willow also wrote "most light to be" where every model has "most likely to be". No accent errors showed up
  that are specific to one local model.
- **Real model errors** are mostly uncommon product names that are in no glossary, and project names split
  into two words. There are also compound splits like "dropdown" vs "drop down", which cost every model
  equally.
- **Parakeet quirks:** "Claude" becomes "clawed", "horoscope" becomes "horror scope", and it occasionally drops
  the space before a "$" amount.

## Method

- **Data, read-only.** Each `Willow\Transcripts\<id>.json` points at its recording through `audioFilePath`,
  `Recordings\<id>.aac` (ADTS AAC, 48 kHz mono). The reference is that JSON's `query` field. 1,080 transcripts
  were found, and 997 were usable after dropping these (no credential file was opened):
  - failed dictations (20)
  - email or scribe AI-rewrite modes (20)
  - fewer than 3 words (14)
  - under 2 s (29)
- **Sample:** 200 clips, seed 20261102, 76 min of audio, decoded to 16 kHz mono with ffmpeg.
  - Clip lengths: min 2.0 s, p10 3.6, p25 6.5, median 11.3, p75 29.0, p90 51.3, max 253.8 s.
  - By length band: 40 clips under 5 s, 50 at 5–10 s, 41 at 10–20 s, 21 at 20–30 s, 33 at 30–60 s and 15
    over 60 s.
- **Scoring:**
  - Normalisation: whisper-normalizer `EnglishTextNormalizer` (lower-case, punctuation, numbers, US/UK
    spelling), plus removal of erm/er/uh/um/ah, okay mapped to ok, and dictionary shortcut replacements mapped
    back to their spoken triggers.
  - WER: jiwer, at corpus level.
  - Confidence intervals: 95%, bootstrapping over clips. Model-vs-model differences are paired bootstraps over
    the same clips.
- **Speed:** each configuration ran in its own process. Audio was preloaded, and the timer ran from the
  `transcribe` call until all text was out.
  - "Cold load" is the model constructor. "First call" is the first 5 s transcription.
  - VRAM is the total-GPU `memory.used` peak minus the pre-load baseline, polled every 100 ms. Per-process
    figures are N/A under WDDM.
- **No-speech clips:** 5 s and 15 s of digital zero, 5 s and 15 s of −60 dBFS white noise, and 6 room-tone
  clips built from his recordings.
- **Downloads** (all from official Hugging Face repos):

  | model | repo | size |
  |---|---|---|
  | large-v3-turbo | `mobiuslabsgmbh/faster-whisper-large-v3-turbo` | 1,547 MB |
  | distil-large-v3.5 | `distil-whisper/distil-large-v3.5-ct2` | 1,446 MB |
  | small.en | `Systran/faster-whisper-small.en` | 464 MB |
  | Parakeet TDT 0.6B v2 | `istupakov/parakeet-tdt-0.6b-v2-onnx`, fp32 + int8 | 3,027 MB |
  | Silero VAD | `istupakov/silero-vad-onnx` | 6 MB |
  | Parakeet for sherpa-onnx | `csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8` | 630 MB |

## Caveats

- **The reference is Willow's output, not ground truth.** It is a cloud model plus AI clean-up, and it has its
  own errors (see "free"/"three"). Absolute WERs are "distance from Willow". Only the relative ranking means
  much, and the top five configurations are within about 0.4 points of each other.
- **Several settings are statistically tied.** On a 200-clip sample, beam 1 vs 5, int8 vs float16, and turbo
  vs Parakeet are all ties. The recommendation rests on ties broken by VRAM, silence behaviour, biasing and
  ecosystem, not on WER alone.
- **Latency moved with other GPU load on the PC.** The baseline swung between 1.2 and 3.4 GB overnight. The
  length profile is the fair head-to-head. Cold-load figures assume the model files are in the OS cache. A
  first load after a reboot reads 1.5 GB from disk on top.
- **Packaging trap: onnxruntime-gpu 1.31.1 is built for CUDA 13 and silently fell back to CPU.** The pinned
  1.23.2 uses the same CUDA 12 / cuDNN 9 wheels as CTranslate2. faster-whisper also pulls the CPU
  `onnxruntime`, which shadows the GPU package. See `bench/README.md`.
- **The Parakeet fp16 conversion failed and was dropped.** onnxconverter-common produced a graph that
  onnxruntime refused to load (a mixed-type `/pre_encode/Add`), and onnxruntime's own converter hits the
  protobuf 2 GB limit.

## Sidecar starting point (tested)

Shipped as `C:\Wren\bench\wren_stt.py`. Running `python wren_stt.py` self-tests it, and it passed on this
machine with the following output:

- load and warm-up: 6.1 s
- 5 s of silence: no text
- 5 s of his speech: 264 ms
- 10 s: 355 ms
- 15 s: 597 ms

It needs `faster-whisper`, `nvidia-cublas-cu12` and `nvidia-cudnn-cu12` (9.x) in a Python 3.12 venv.

```python
import os, sys
from pathlib import Path
import numpy as np

def _add_cuda_dlls():
    # nvidia-*-cu12 wheels keep their DLLs in site-packages/nvidia/*/bin, which Windows does not search.
    for bin_dir in sorted((Path(sys.prefix) / "Lib" / "site-packages" / "nvidia").glob("*/bin")):
        os.add_dll_directory(str(bin_dir))
        os.environ["PATH"] = str(bin_dir) + os.pathsep + os.environ.get("PATH", "")

class WrenSTT:
    def __init__(self, model_dir, glossary=(), beam_size=1, compute_type="int8_float16"):
        _add_cuda_dlls()
        from faster_whisper import WhisperModel
        self.model = WhisperModel(str(model_dir), device="cuda", compute_type=compute_type, local_files_only=True)
        self.beam_size = beam_size
        terms = [t.strip() for t in glossary if t and t.strip()]
        self.prompt = ("Glossary: " + ", ".join(terms) + ".") if terms else None
        self.transcribe(np.zeros(16000, dtype=np.float32))  # warm-up

    def transcribe(self, audio: np.ndarray) -> str:  # 1-D float32, 16 kHz mono, [-1, 1]
        segments, _ = self.model.transcribe(
            np.asarray(audio, dtype=np.float32).reshape(-1), language="en", beam_size=self.beam_size,
            vad_filter=True, condition_on_previous_text=False, initial_prompt=self.prompt)
        return " ".join(s.text.strip() for s in segments).strip()

stt = WrenSTT(r"C:\Wren\bench\models\large-v3-turbo", glossary=["Claude", "Claude Code", "Overseer"])
print(stt.transcribe(np.zeros(5 * 16000, dtype=np.float32)))  # -> "" (silence)
```

How to rerun everything: `C:\Wren\bench\README.md`.
