<#
.SYNOPSIS
  Live end-to-end test of Wren (Task 8, spec §13): the real app, the real hook, a fake microphone,
  pasting into scripts/test-page.html. Writes docs/live-test-report.md.

.DESCRIPTION
  Refuses (prints SKIPPED, exit 3) if the session is locked, or Willow or Glaido is running (they
  would react to the chord too), or another Wren is running.

  Builds the app with its front end (npx tauri build --debug --no-bundle) and the test driver
  (cargo build --example live_test) with a fresh WREN_BUILD_ID, copies both to a private folder and
  checks each copy carries that WREN_BUILD_ID (the build folder is shared with other builders, so
  whatever is in .cargo-target\debug at that moment may be someone else's). With -Wren <exe> (for
  example the installed %LOCALAPPDATA%\Programs\Wren\wren.exe) only the driver is built and that exe
  is tested as it is. Then it runs src-tauri/examples/live_test.rs, which:
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
  powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1 -Wren "$env:LOCALAPPDATA\Programs\Wren\wren.exe"
    (the installed app; add -GrabDir <folder> to keep screen grabs of the popup's states)
  powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1 -Task9 -Report $env:TEMP\wren-task9.md
    (Task 9 only: fix a word after a paste in the textarea and in the composer and check Wren learns
    it with the "Learned" toast; then dictate with nothing editable focused and check it is copied,
    not pasted)
#>
param(
    [int]$Holds = 10,
    [int]$Taps = 50,
    [switch]$NoBuild,
    [switch]$Task9,
    [string]$Report = '',
    [string]$WorkDir = '',
    [string]$Wren = '',
    [string]$GrabDir = ''
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
    'SKIPPED: another Wren is running (quit it from the tray first)'
    exit 3
}

if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = 'C:\Wren\.cargo-target' }
$target = $env:CARGO_TARGET_DIR
# Outside %LOCALAPPDATA% on purpose (MSIX redirection when launched from the Claude desktop app).
if (-not $WorkDir) { $WorkDir = Join-Path (Split-Path $repo -Parent) 'wren-live-test' }
New-Item -ItemType Directory -Force (Join-Path $WorkDir 'bin') | Out-Null

function Test-HasMarker([string]$Path, [string]$Marker) {
    $text = [Text.Encoding]::GetEncoding(28591).GetString([IO.File]::ReadAllBytes($Path))
    return $text.Contains($Marker)
}

# ---- build ----
if (-not $NoBuild) {
    # A fresh id for this build; the copies below must carry it (see .DESCRIPTION).
    $env:WREN_BUILD_ID = 'live-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + ([guid]::NewGuid().ToString('N').Substring(0, 8))
    Push-Location $repo
    try {
        if (-not $Wren) {
            & npx tauri build --debug --no-bundle
            if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
        }
        Push-Location (Join-Path $repo 'src-tauri')
        try {
            & cargo build --example live_test
            if ($LASTEXITCODE -ne 0) { throw 'building the live test failed' }
        } finally { Pop-Location }
    } finally { Pop-Location }
    $copies = @(, @('debug\examples\live_test.exe', 'bin\live_test.exe'))
    if (-not $Wren) { $copies += , @('debug\wren.exe', 'bin\wren.exe') }
    foreach ($c in $copies) {
        $to = Join-Path $WorkDir $c[1]
        Copy-Item (Join-Path $target $c[0]) $to -Force
        if (-not (Test-HasMarker $to "wren-build-id:$env:WREN_BUILD_ID")) {
            throw "$($c[0]) in $target is not this build (another builder overwrote it); run again"
        }
    }
    "built and copied: $env:WREN_BUILD_ID"
}

$wrenExe = if ($Wren) { (Resolve-Path $Wren).Path } else { Join-Path $WorkDir 'bin\wren.exe' }
$driver = Join-Path $WorkDir 'bin\live_test.exe'
foreach ($f in @($wrenExe, $driver)) { if (-not (Test-Path $f)) { throw "missing $f (run without -NoBuild)" } }
"testing: $wrenExe"

# The driver finds the fixtures and the test page through the source tree it was built from.
if (-not $Report) { $Report = Join-Path $repo 'docs\live-test-report.md' }
$extra = @()
if ($Task9) { $extra += '--task9' }
if ($GrabDir) { $extra += @('--grab-dir', $GrabDir) }
& $driver --wren $wrenExe --home (Join-Path $WorkDir 'home') --report $Report --holds $Holds --taps $Taps @extra
exit $LASTEXITCODE
