# Wren STT benchmark

Benchmarks local speech-to-text models on the owner's own Willow Voice recordings, so Wren's sidecar
uses the model that is most accurate on *his* voice at a latency that feels instant. Results and the
recommendation are in `../docs/research/stt-benchmark.md`.

Everything runs on this PC. `.venv/`, `models/` and `out/` are git-ignored: `out/` holds decoded audio
and per-clip transcripts, which are his dictated text, so it never leaves the machine.

## What is here

| file | does |
|---|---|
| `download.py` | fetches the candidate models from Hugging Face into `models/` and prints each size |
| `prepare.py` | pairs Willow recordings with transcripts, draws the fixed-seed sample, decodes audio to 16 kHz WAV, builds the no-speech clips and the dictionary-term subset |
| `bench.py` | runs one model configuration over a subset; records per-clip text, warm latency, cold load, first-call time, VRAM and silence outputs |
| `run_all.sh` | runs every configuration, each in its own process, then the VRAM/latency-by-length profiles |
| `vram_profile.py` | VRAM and warm latency for exact 5–240 s inputs, one config (the fair head-to-head) |
| `sherpa_hotwords.py` | Parakeet hotword-biasing test via sherpa-onnx (CPU); runs in the separate `.venv-sherpa` |
| `score.py` | normalised WER (with bootstrap 95% CI), latency fits, hallucination counts, biasing tables -> `out/tables.md` |
| `inspect_disagreements.py` | prints the clips where models disagree most, for reading by eye (local only) |
| `wren_stt.py` | the recommended loader + `transcribe(float32 16 kHz mono buffer)`: the start of Wren's sidecar; `python wren_stt.py` self-tests it |

## Rerun from scratch

```bash
cd C:/Wren/bench
uv venv --python 3.12 .venv
uv pip install --python .venv/Scripts/python.exe faster-whisper nvidia-cublas-cu12 "nvidia-cudnn-cu12>=9,<10" \
    "onnx-asr[gpu,hub]" jiwer whisper-normalizer soundfile numpy huggingface_hub \
    nvidia-cuda-runtime-cu12 nvidia-cufft-cu12 nvidia-curand-cu12
# faster-whisper pulls the CPU onnxruntime, which overwrites the GPU build (same import name). Keep only the GPU one,
# and pin 1.23.2: onnxruntime-gpu 1.31.1 is built for CUDA 13 (cublas64_13.dll) and silently falls back to CPU here,
# while 1.23.2 uses the same CUDA 12 / cuDNN 9 wheels as CTranslate2.
uv pip uninstall --python .venv/Scripts/python.exe onnxruntime onnxruntime-gpu
uv pip install --python .venv/Scripts/python.exe --no-deps onnxruntime-gpu==1.23.2

.venv/Scripts/python.exe download.py          # ~6.5 GB total
.venv/Scripts/python.exe prepare.py           # needs ffmpeg/ffprobe on PATH
bash run_all.sh                               # ~2.5 h for the full suite on an RTX 3060 Ti (SKIP_DONE=1 resumes)
# optional: Parakeet hotwords through sherpa-onnx (CPU, ~35 min)
uv venv --python 3.12 .venv-sherpa && uv pip install --python .venv-sherpa/Scripts/python.exe sherpa-onnx soundfile numpy
.venv/Scripts/python.exe -c "from huggingface_hub import snapshot_download as s; s('csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8', local_dir='models/sherpa-parakeet-tdt-0.6b-v2-int8')"
.venv-sherpa/Scripts/python.exe sherpa_hotwords.py
.venv/Scripts/python.exe score.py             # tables -> out/tables.md
.venv/Scripts/python.exe wren_stt.py          # self-test of the recommended sidecar loader
```

Single configuration: `bash run_all.sh turbo-i8f16-b5` or
`.venv/Scripts/python.exe bench.py turbo-i8f16-b5 --subset terms`. `bench.py --list` lists them all.

## Notes

- Inputs are read-only: `%APPDATA%/willow-voice/Recordings/<id>.aac` (ADTS AAC, 48 kHz mono), paired via
  `Transcripts/<id>.json` -> `audioFilePath`; the reference text is that JSON's `query`. Only
  `status=completed`, `llmType=normal` dictations with >= 3 words and >= 2 s of audio are used.
  `supabase-session.json` and other credential files are never opened.
- Willow's text is a cloud model's output (possibly AI-cleaned), so WER here is "distance from Willow", an
  imperfect reference. Compare models relative to each other.
- VRAM is total-GPU `memory.used` peak minus the baseline just before load (per-process numbers are N/A under
  Windows WDDM), so other apps changing their usage mid-run adds noise of a few tens of MB.
- Close GPU-heavy apps (games, wallpaper engines, video) before a run for steadier latency numbers.
- `.venv-sherpa/` is git-ignored like `.venv/`.
- The sample seed is in `prepare.py` (`SEED`); changing it, or new Willow recordings, changes the sample.
