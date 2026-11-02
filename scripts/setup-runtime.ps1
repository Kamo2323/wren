<#
.SYNOPSIS
  Builds Wren's speech runtime: a private Python 3.12, the sidecar venv, and the model files.

.DESCRIPTION
  Everything lands under %LOCALAPPDATA%\Wren:
    runtime\python   uv-managed CPython 3.12 (Wren's own copy; no system Python is used)
    runtime\.venv    the sidecar venv, built from sidecar\pyproject.toml + sidecar\uv.lock (frozen)
    models\<name>    CTranslate2 model folders from the official Hugging Face repos, pinned revisions

  Idempotent: re-running checks each part and only fetches what is missing or wrong. This is the only
  step that touches the internet; Wren itself never downloads anything (local_files_only=True).

.PARAMETER SidecarDir
  Folder holding pyproject.toml and uv.lock. Defaults to ..\sidecar next to this script.

.PARAMETER ModelSource
  Optional folder that already holds copies of the model folders (for example C:\Wren\bench\models).
  A missing model is seeded from there first; the Hugging Face check then confirms every file matches
  the pinned revision and downloads anything that does not.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\setup-runtime.ps1
#>
param(
    [string]$SidecarDir = (Join-Path $PSScriptRoot '..\sidecar'),
    [string]$ModelSource = '',
    [string]$WrenRoot = (Join-Path $env:LOCALAPPDATA 'Wren')
)

$ErrorActionPreference = 'Stop'
$PythonVersion = '3.12'

# name -> repo + the revision the benchmark tested (docs/research/stt-benchmark.md).
$ModelRepos = [ordered]@{
    'large-v3-turbo' = @{ Repo = 'mobiuslabsgmbh/faster-whisper-large-v3-turbo'; Revision = '0a363e9161cbc7ed1431c9597a8ceaf0c4f78fcf' }
    'small.en'       = @{ Repo = 'Systran/faster-whisper-small.en'; Revision = 'd1d751a5f8271d482d14ca55d9e2deeebbae577f' }
}

function Write-Step([string]$msg) { Write-Host "==> $msg" -ForegroundColor Cyan }
function Get-FolderMB([string]$path) {
    if (-not (Test-Path $path)) { return 0 }
    $bytes = (Get-ChildItem $path -Recurse -File -Force | Measure-Object Length -Sum).Sum
    if ($null -eq $bytes) { return 0 }
    return [math]::Round($bytes / 1MB)
}
function Invoke-Native([string]$exe, [string[]]$arguments) {
    & $exe @arguments
    if ($LASTEXITCODE -ne 0) { throw "$exe $($arguments -join ' ') failed with exit code $LASTEXITCODE" }
}

$SidecarDir = (Resolve-Path $SidecarDir).Path
if (-not (Test-Path (Join-Path $SidecarDir 'uv.lock'))) { throw "No uv.lock in $SidecarDir" }
$uv = (Get-Command uv -ErrorAction SilentlyContinue)
if ($null -eq $uv) { throw 'uv is not installed. Install it with: winget install astral-sh.uv' }

$runtime = Join-Path $WrenRoot 'runtime'
$venv = Join-Path $runtime '.venv'
$venvPython = Join-Path $venv 'Scripts\python.exe'
$modelsDir = Join-Path $WrenRoot 'models'
New-Item -ItemType Directory -Force $runtime, $modelsDir | Out-Null

# Wren's own interpreter, kept inside runtime\ so removing that folder removes all of it.
$env:UV_PYTHON_INSTALL_DIR = Join-Path $runtime 'python'
$env:UV_PYTHON_PREFERENCE = 'only-managed'
$env:UV_PROJECT_ENVIRONMENT = $venv
$env:UV_LINK_MODE = 'copy'   # the venv must not hard-link into uv's cache (cache cleans would break it)
$env:HF_HUB_DISABLE_PROGRESS_BARS = '1'

Write-Step "Python $PythonVersion (uv-managed, in $($env:UV_PYTHON_INSTALL_DIR))"
# --no-bin / --no-registry: Wren's interpreter is private; it never lands on PATH or in the registry.
Invoke-Native 'uv' @('python', 'install', '--no-bin', '--no-registry', $PythonVersion)

$venvOk = $false
if (Test-Path $venvPython) {
    $ver = & $venvPython -c "import sys; print('%d.%d' % sys.version_info[:2])" 2>$null
    $venvOk = ($LASTEXITCODE -eq 0 -and $ver -eq $PythonVersion)
}
if (-not $venvOk) {
    Write-Step "Creating venv $venv"
    Invoke-Native 'uv' @('venv', '--clear', '--python', $PythonVersion, $venv)
} else {
    Write-Step "Venv already present ($venv)"
}

# --compile-bytecode: the app runs with PYTHONDONTWRITEBYTECODE, so .pyc files must exist up front
# or every launch recompiles numpy, av, ctranslate2... from source.
Write-Step 'Installing locked packages (uv sync --frozen --compile-bytecode)'
Invoke-Native 'uv' @('sync', '--project', $SidecarDir, '--frozen', '--no-dev', '--no-install-project', '--compile-bytecode')

Write-Step 'Models'
$fetch = @'
import sys
from pathlib import Path
from huggingface_hub import snapshot_download
name, repo, revision, dest = sys.argv[1:5]
path = snapshot_download(repo, revision=revision, local_dir=dest)
p = Path(path)
if not (p / "model.bin").is_file():
    sys.exit(f"{name}: model.bin missing after download")
mb = sum(f.stat().st_size for f in p.rglob("*") if f.is_file() and ".cache" not in f.parts) / 2**20
print(f"{name}: {repo}@{revision[:8]} ok, {mb:.0f} MB")
'@
$fetchPy = Join-Path $runtime 'fetch_model.py'
Set-Content -Path $fetchPy -Value $fetch -Encoding ascii

foreach ($name in $ModelRepos.Keys) {
    $info = $ModelRepos[$name]
    $dest = Join-Path $modelsDir $name
    $marker = Join-Path $dest '.wren-revision'
    if ((Test-Path $marker) -and ((Get-Content $marker -Raw).Trim() -eq $info.Revision) -and (Test-Path (Join-Path $dest 'model.bin'))) {
        Write-Host "$name already at $($info.Revision.Substring(0,8)) ($(Get-FolderMB $dest) MB)"
        continue
    }
    if ($ModelSource -and -not (Test-Path (Join-Path $dest 'model.bin'))) {
        $src = Join-Path $ModelSource $name
        if (Test-Path (Join-Path $src 'model.bin')) {
            Write-Host "Seeding $name from $src"
            # robocopy exit codes below 8 mean success.
            & robocopy $src $dest /E /NFL /NDL /NJH /NJS /NP | Out-Null
            if ($LASTEXITCODE -ge 8) { throw "robocopy $src failed ($LASTEXITCODE)" }
        }
    }
    $before = Get-FolderMB $dest
    Invoke-Native $venvPython @('-I', $fetchPy, $name, $info.Repo, $info.Revision, $dest)
    $after = Get-FolderMB $dest
    Write-Host "  $name downloaded $([math]::Max(0, $after - $before)) MB (folder now $after MB)"
    Set-Content -Path $marker -Value $info.Revision -Encoding ascii
}
Remove-Item $fetchPy -Force

Write-Step 'Checking the runtime'
$check = @'
import os, sys
from pathlib import Path
root = Path(sys.prefix) / "Lib" / "site-packages" / "nvidia"
for sub in ("cublas", "cudnn"):
    b = root / sub / "bin"
    if b.is_dir():
        os.add_dll_directory(str(b))
        os.environ["PATH"] = str(b) + os.pathsep + os.environ.get("PATH", "")
import ctranslate2, faster_whisper
print(f"faster-whisper {faster_whisper.__version__}, ctranslate2 {ctranslate2.__version__}, cuda devices: {ctranslate2.get_cuda_device_count()}")
'@
# Run from a file: Windows PowerShell 5.1 strips double quotes from native arguments.
$checkPy = Join-Path $runtime 'check_runtime.py'
Set-Content -Path $checkPy -Value $check -Encoding ascii
try { Invoke-Native $venvPython @('-I', $checkPy) } finally { Remove-Item $checkPy -Force }

Write-Host ''
Write-Host "Runtime: $runtime ($(Get-FolderMB $runtime) MB)"
Write-Host "Models:  $modelsDir ($(Get-FolderMB $modelsDir) MB)"
Write-Host 'Wren speech runtime ready.' -ForegroundColor Green
