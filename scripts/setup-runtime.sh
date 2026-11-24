#!/usr/bin/env bash
# Builds Wren's macOS speech runtime: a private Python 3.12, the MLX sidecar venv, and the model files.
#
# Everything lands under ~/Library/Application Support/Wren (or $WREN_ROOT):
#   runtime/python   uv-managed CPython 3.12 (Wren's own copy; no system Python is used)
#   runtime/.venv    the sidecar venv, built from sidecar/macos/pyproject.toml + uv.lock
#   models/<name>    MLX Whisper & fallback model folders from official Hugging Face repos (pinned)
#
# Idempotent: re-running checks each part and only fetches what is missing or outdated.
# This is the only step that touches the internet; Wren itself runs strictly offline.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
MACOS_PROJECT="${SIDECAR_MACOS_DIR:-$REPO_ROOT/sidecar/macos}"
WREN_ROOT="${WREN_ROOT:-$HOME/Library/Application Support/Wren}"
PYTHON_VERSION="3.12"

# Pinned revisions verified from Hugging Face API.
PRIMARY_NAME="large-v3-turbo-q4"
PRIMARY_REPO="mlx-community/whisper-large-v3-turbo-q4"
PRIMARY_REV="660c343bbf4e52ac257f0b7d952e5388e6f93bef"

FALLBACK_NAME="small.en"
FALLBACK_REPO="Systran/faster-whisper-small.en"
FALLBACK_REV="d1d751a5f8271d482d14ca55d9e2deeebbae577f"

step() { printf '\n==> %s\n' "$1"; }
folder_mb() {
  if [ -d "$1" ]; then
    du -sm "$1" 2>/dev/null | awk '{print $1}'
  else
    echo 0
  fi
}

if ! command -v uv >/dev/null 2>&1; then
  echo "ERROR: uv is not installed. Install it with: brew install uv" >&2
  exit 1
fi

if [ ! -f "$MACOS_PROJECT/pyproject.toml" ]; then
  echo "ERROR: missing $MACOS_PROJECT/pyproject.toml" >&2
  exit 1
fi

RUNTIME="$WREN_ROOT/runtime"
VENV="$RUNTIME/.venv"
VENV_PYTHON="$VENV/bin/python"
MODELS_DIR="$WREN_ROOT/models"
mkdir -p "$RUNTIME" "$MODELS_DIR"

export UV_PYTHON_INSTALL_DIR="$RUNTIME/python"
export UV_PYTHON_PREFERENCE="only-managed"
export UV_PROJECT_ENVIRONMENT="$VENV"
export UV_LINK_MODE="copy"
export HF_HUB_DISABLE_PROGRESS_BARS="1"

step "Python $PYTHON_VERSION (uv-managed, in $UV_PYTHON_INSTALL_DIR)"
uv python install --no-bin "$PYTHON_VERSION"

VENV_OK=0
if [ -x "$VENV_PYTHON" ]; then
  VER="$("$VENV_PYTHON" -c "import sys; print('%d.%d' % sys.version_info[:2])" 2>/dev/null || true)"
  if [ "$VER" = "$PYTHON_VERSION" ]; then
    VENV_OK=1
  fi
fi

if [ "$VENV_OK" -eq 0 ]; then
  step "Creating venv $VENV"
  uv venv --clear --python "$PYTHON_VERSION" "$VENV"
else
  step "Venv already present ($VENV)"
fi

SYNC_FLAGS=(--project "$MACOS_PROJECT" --no-dev --no-install-project --compile-bytecode)
if [ -f "$MACOS_PROJECT/uv.lock" ]; then
  SYNC_FLAGS=(--frozen "${SYNC_FLAGS[@]}")
fi
step "Installing macOS sidecar packages (uv sync ${SYNC_FLAGS[*]})"
uv sync "${SYNC_FLAGS[@]}"

step "Models"
FETCH_PY="$RUNTIME/fetch_model.py"
cat > "$FETCH_PY" <<'PYEOF'
import sys
from pathlib import Path
from huggingface_hub import snapshot_download

name, repo, revision, dest, required_any = sys.argv[1:6]
required = required_any.split("|")
path = Path(snapshot_download(repo, revision=revision, local_dir=dest))
if not any((path / f).is_file() for f in required):
    sys.exit(f"{name}: none of {required} found after download in {path}")
mb = sum(f.stat().st_size for f in path.rglob("*") if f.is_file() and ".cache" not in f.parts) / 2**20
print(f"{name}: {repo}@{revision[:8]} ok, {mb:.0f} MB")
PYEOF

download_model() {
  local name="$1" repo="$2" rev="$3" req="$4"
  local dest="$MODELS_DIR/$name"
  local marker="$dest/.wren-revision"
  if [ -f "$marker" ] && [ "$(tr -d '\r\n ' < "$marker")" = "$rev" ]; then
    local present=0
    IFS='|' read -r -a files <<< "$req"
    for f in "${files[@]}"; do
      if [ -f "$dest/$f" ]; then present=1; break; fi
    done
    if [ "$present" -eq 1 ]; then
      echo "$name already at ${rev:0:8} ($(folder_mb "$dest") MB)"
      return 0
    fi
  fi
  local before after delta
  before="$(folder_mb "$dest")"
  "$VENV_PYTHON" -I "$FETCH_PY" "$name" "$repo" "$rev" "$dest" "$req"
  after="$(folder_mb "$dest")"
  delta=$((after - before))
  if [ "$delta" -lt 0 ]; then delta=0; fi
  echo "  $name downloaded ${delta} MB (folder now ${after} MB)"
  printf '%s\n' "$rev" > "$marker"
}

download_model "$PRIMARY_NAME" "$PRIMARY_REPO" "$PRIMARY_REV" "weights.npz|weights.safetensors"
download_model "$FALLBACK_NAME" "$FALLBACK_REPO" "$FALLBACK_REV" "model.bin"
rm -f "$FETCH_PY"

step "Checking the runtime"
"$VENV_PYTHON" -I -c '
import platform
import mlx.core as mx
import mlx_whisper
import faster_whisper
print(f"arch={platform.machine()}, mlx={mx.__version__}, default_device={mx.default_device()}, faster-whisper={faster_whisper.__version__}")
'

echo ""
echo "Runtime: $RUNTIME ($(folder_mb "$RUNTIME") MB)"
echo "Models:  $MODELS_DIR ($(folder_mb "$MODELS_DIR") MB)"
echo "Wren macOS speech runtime ready."
