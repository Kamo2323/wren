#!/usr/bin/env bash
# Runs every benchmark configuration in its own process (clean CUDA context for VRAM/cold-load numbers).
# Usage: bash run_all.sh [config ...]   (no args = full suite). Logs to out/run.log.
#        SKIP_DONE=1 bash run_all.sh    resumes, skipping configs that already have a full result.
cd "$(dirname "$0")"
PY=.venv/Scripts/python.exe
MAIN="turbo-f16-b5 turbo-i8f16-b5 distil-f16-b5 distil-i8f16-b5 smallen-i8f16-b5 parakeet-fp32 parakeet-int8 parakeet-fp32-samereq parakeet-fp32-vad30
      turbo-i8f16-b1 turbo-i8f16-b5-vad turbo-i8f16-b5-nots turbo-i8f16-batch8 distil-f16-b1 distil-f16-b5-vad"
BIAS="turbo-i8f16-b5-prompt-dict turbo-i8f16-b5-hotwords-dict turbo-i8f16-b5-prompt-vocab turbo-i8f16-b5-hotwords-vocab
      distil-f16-b5-prompt-dict distil-f16-b5-hotwords-dict distil-f16-b5-prompt-vocab distil-f16-b5-hotwords-vocab"
COMBO="turbo-i8f16-b5-vad-prompt-vocab turbo-i8f16-b1-vad-prompt-vocab"
TERMS="turbo-i8f16-b5 distil-f16-b5 parakeet-fp32 $BIAS $COMBO"
if [ $# -gt 0 ]; then MAIN="$*"; BIAS=""; COMBO=""; TERMS=""; fi
run() {  # $1 config, $2 subset
  local s="out/results/$1__$2.summary.json"
  if [ -n "$SKIP_DONE" ] && [ -f "$s" ] && ! grep -q '"n": [1-9],' "$s"; then echo "skip $1 $2"; return; fi
  echo "== $1 $2" | tee -a out/run.log
  $PY bench.py "$1" --subset "$2" 2>&1 | grep -a '^{\|Error\|Traceback' | tail -3 | tee -a out/run.log
}
for c in $MAIN $BIAS $COMBO; do run "$c" sample; done
for c in $TERMS; do run "$c" terms; done
# VRAM / latency against dictation length for the finalists (sherpa_hotwords.py runs separately, see README)
if [ -z "$NO_PROFILE" ] && [ $# -eq 0 ]; then
  for c in turbo-i8f16-b1-vad-prompt-vocab turbo-i8f16-b5-vad-prompt-vocab distil-i8f16-b5 parakeet-fp32 parakeet-fp32-vad30; do
    $PY vram_profile.py "$c" 2>&1 | grep -a '^{' | tail -1 | tee -a out/run.log
  done
fi
