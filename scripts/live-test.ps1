<#
.SYNOPSIS
  Live end-to-end test of Wren (Task 8, spec §13): the real app, the real hook, a fake microphone,
  pasting into scripts/test-page.html. Writes docs/live-test-report.md.

.DESCRIPTION
  Refuses (prints SKIPPED, exit 3) if the session is locked, or Willow or Glaido is running (they
  would react to the chord too), or another Wren is running.

  Builds the app with its front end (npx tauri build --debug --no-bundle) and the test driver
  (cargo build --example live_test), copies wren.exe to a private folder (the build folder is shared
  with other builders), then runs src-tauri/examples/live_test.rs, which:
    - starts Wren hidden with WREN_HOME = a temp folder, WREN_RUNTIME_ROOT = the installed engine,
      WREN_TEST_AUDIO = tests/fixtures/weather.wav;
    - opens the test page in an Edge app window and focuses it without sending keys;
    - injects 3 s chord holds, a hands-free double-tap run and a hold into the chat composer, and
      checks the page got the words, the foreground window never changed, an image on the
      clipboard survives, and the release-to-paste time;
    - 50 chord taps (Start must never open), Ctrl+Win+Right passthrough;
    - restores the user's clipboard, checks clipboard history and the logs, writes the report.
  Keys are only ever sent while the test page is the foreground window (never claude.exe, never
  Notepad).

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1
  powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1 -Holds 3 -Taps 10 -NoBuild
#>
param(
    [int]$Holds = 10,
    [int]$Taps = 50,
    [switch]$NoBuild,
    [string]$WorkDir = ''
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

# ---- refusals ----
if (Get-Process -Name LogonUI -ErrorAction SilentlyContinue) {
    'SKIPPED: the session is locked'
    exit 3
}
$rivals = @(Get-Process | Where-Object { $_.ProcessName -match 'willow|glaido' } | Select-Object -ExpandProperty ProcessName -Unique)
if ($rivals.Count -gt 0) {
    "SKIPPED: $($rivals -join ', ') running (it would react to the chord too)"
    exit 3
}
if (Get-Process -Name wren -ErrorAction SilentlyContinue) {
    'SKIPPED: another Wren is running'
    exit 3
}

if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\Wren\.cargo-target' }
$target = $env:CARGO_TARGET_DIR
# Outside %LOCALAPPDATA% on purpose (MSIX redirection when launched from the Claude desktop app).
if (-not $WorkDir) { $WorkDir = Join-Path (Split-Path $repo -Parent) 'wren-live-test' }
New-Item -ItemType Directory -Force (Join-Path $WorkDir 'bin') | Out-Null

# ---- build ----
if (-not $NoBuild) {
    Push-Location $repo
    try {
        & npx tauri build --debug --no-bundle
        if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
        Push-Location (Join-Path $repo 'src-tauri')
        try {
            & cargo build --example live_test
            if ($LASTEXITCODE -ne 0) { throw 'building the live test failed' }
        } finally { Pop-Location }
    } finally { Pop-Location }
    Copy-Item (Join-Path $target 'debug\wren.exe') (Join-Path $WorkDir 'bin\wren.exe') -Force
    Copy-Item (Join-Path $target 'debug\examples\live_test.exe') (Join-Path $WorkDir 'bin\live_test.exe') -Force
}

$wren = Join-Path $WorkDir 'bin\wren.exe'
$driver = Join-Path $WorkDir 'bin\live_test.exe'
foreach ($f in @($wren, $driver)) { if (-not (Test-Path $f)) { throw "missing $f (run without -NoBuild)" } }

# The driver finds the fixtures and the test page through the source tree it was built from.
& $driver --wren $wren --home (Join-Path $WorkDir 'home') --report (Join-Path $repo 'docs\live-test-report.md') --holds $Holds --taps $Taps
exit $LASTEXITCODE
