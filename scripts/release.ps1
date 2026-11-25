<#
.SYNOPSIS
  Build Wren's installer, install it for this user, and run the installed self-test (Task 10).

.DESCRIPTION
  One command, safe to re-run (a second run upgrades in place):
    1. checks the NSIS template still matches the Tauri CLI (node scripts/nsis-template.mjs --check);
    2. builds the front end and the release app with a fresh WREN_BUILD_ID, and bundles the NSIS
       per-user installer (npx tauri build --bundles nsis);
    3. copies the installer to a staging folder, asks a running Wren to quit (wren --quit), and
       installs silently (/S) to %LOCALAPPDATA%\Programs\Wren;
    4. checks the installed wren.exe is the build just made (its WREN_BUILD_ID), and that nothing
       landed in the Claude app's package cache;
    5. runs the installed wren.exe --selftest and prints its result (also kept in
       %LOCALAPPDATA%\Wren\logs\selftest.txt);
    6. starts Wren again in the tray if it was running before (or with -Launch).

  Wren's data (%APPDATA%\Wren, %LOCALAPPDATA%\Wren) is never touched by the installer.

  The installer, the self-test and the launch are started through WMI (Win32_Process.Create), so they
  run outside the Claude desktop app's MSIX container even when this script is started from it:
  otherwise their new files under %LOCALAPPDATA% would be redirected into Claude's package cache.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\release.ps1
  powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Launch
  powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -NoBuild     (reinstall the last build)
#>
param(
    [switch]$NoBuild,
    [switch]$NoInstall,
    [switch]$NoSelftest,
    [switch]$Launch,
    [string]$StageDir = ''
)
# Defaults are inside this checkout (git-ignored), so any clone builds without owner-specific paths.
# Override with -StageDir / $env:WREN_STAGE_DIR and $env:CARGO_TARGET_DIR.
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$installDir = Join-Path $env:LOCALAPPDATA 'Programs\Wren'
$exe = Join-Path $installDir 'wren.exe'
$dataRoot = Join-Path $env:LOCALAPPDATA 'Wren'
$settingsRoot = Join-Path $env:APPDATA 'Wren'
if (-not $env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR = Join-Path $repo '.cargo-target' }
$target = $env:CARGO_TARGET_DIR
# Outside %LOCALAPPDATA% on purpose (see MSIX above).
if (-not $StageDir -and $env:WREN_STAGE_DIR) { $StageDir = $env:WREN_STAGE_DIR }
if (-not $StageDir) { $StageDir = Join-Path $repo '.release-out' }
New-Item -ItemType Directory -Force $StageDir | Out-Null
$version = (Get-Content (Join-Path $repo 'src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json).version
$setupName = "Wren_${version}_x64-setup.exe"
$staged = Join-Path $StageDir $setupName
$buildIdFile = Join-Path $StageDir 'build-id.txt'

function Step($text) { Write-Host ''; Write-Host "== $text" }

# Runs batch lines outside the MSIX container (WMI), waits for them, returns the last exit code.
function Invoke-Outside([string[]]$Lines, [string]$Name, [int]$TimeoutSec = 300) {
    $cmd = Join-Path $StageDir "$Name.cmd"
    $exitFile = Join-Path $StageDir "$Name.exit.txt"
    Remove-Item $exitFile -ErrorAction SilentlyContinue
    Set-Content -Path $cmd -Encoding Ascii -Value (@('@echo off') + $Lines + @(">`"$exitFile`" echo %errorlevel%"))
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = "cmd.exe /c `"$cmd`"" }
    if ($r.ReturnValue -ne 0) { throw "WMI could not start $Name (code $($r.ReturnValue))" }
    $t0 = Get-Date
    while (Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue) {
        if (((Get-Date) - $t0).TotalSeconds -gt $TimeoutSec) { throw "$Name did not finish within $TimeoutSec s" }
        Start-Sleep -Milliseconds 250
    }
    if (-not (Test-Path $exitFile)) { throw "$Name ended without an exit code" }
    return [int]((Get-Content $exitFile -Raw).Trim())
}

function Test-HasMarker([string]$Path, [string]$Marker) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    $text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
    return $text.Contains($Marker)
}

# Folders the Claude app's container would have redirected Wren's writes into.
function Get-PackageCacheStrays {
    $found = @()
    foreach ($pkg in Get-ChildItem (Join-Path $env:LOCALAPPDATA 'Packages') -Directory -Filter 'Claude_*' -ErrorAction SilentlyContinue) {
        foreach ($rel in @('LocalCache\Local\Programs\Wren', 'LocalCache\Local\Wren', 'LocalCache\Roaming\Wren')) {
            $p = Join-Path $pkg.FullName $rel
            if (Test-Path $p) { $found += $p }
        }
    }
    return $found
}

# ---- 1-2. build ----
if (-not $NoBuild) {
    Step 'NSIS template'
    & node (Join-Path $repo 'scripts\nsis-template.mjs') --check
    if ($LASTEXITCODE -ne 0) { throw 'the NSIS template is out of date (see above)' }

    Step 'Build (front end + release app + NSIS installer)'
    $env:WREN_BUILD_ID = 'rel-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '-' + ([guid]::NewGuid().ToString('N').Substring(0, 8))
    Write-Host "WREN_BUILD_ID = $env:WREN_BUILD_ID"
    # Outside the container too: the bundler refreshes its NSIS tools in %LOCALAPPDATA%	auri,
    # which fails inside it ("reparse point", os error 4395).
    $buildLog = Join-Path $StageDir 'build.log'
    Write-Host "building (log: $buildLog) ..."
    $code = Invoke-Outside @(
        "cd /d `"$repo`"",
        "set CARGO_TARGET_DIR=$target",
        "set WREN_BUILD_ID=$env:WREN_BUILD_ID",
        "call npx tauri build --bundles nsis > `"$buildLog`" 2>&1"
    ) 'build' 3600
    Get-Content $buildLog | Where-Object { $_ -match 'Finished|Built application|Finished \d+ bundle|\.exe$|Warn|Error|error' } | ForEach-Object { Write-Host "  $_" }
    if ($code -ne 0) { Get-Content $buildLog -Tail 30 | ForEach-Object { Write-Host "  $_" }; throw "tauri build failed (exit $code)" }
    $built = Join-Path $target "release\bundle\nsis\$setupName"
    if (-not (Test-Path $built)) { throw "the installer was not produced at $built" }
    Copy-Item $built $staged -Force
    Set-Content -Path $buildIdFile -Value $env:WREN_BUILD_ID -Encoding Ascii
    Write-Host "installer: $staged ($([math]::Round((Get-Item $staged).Length / 1MB, 1)) MB)"
}
if (-not (Test-Path $staged) -or -not (Test-Path $buildIdFile)) { throw "no staged installer in $StageDir (run without -NoBuild)" }
$buildId = (Get-Content $buildIdFile -Raw).Trim()
if ($NoInstall) { Write-Host "built $buildId; not installed (-NoInstall)"; exit 0 }

# ---- 3. install ----
Step 'Install'
$wasRunning = [bool](Get-Process -Name wren -ErrorAction SilentlyContinue)
if ($wasRunning) {
    Write-Host 'Wren is running: asking it to quit'
    if (Test-Path $exe) { [void](Invoke-Outside "`"$exe`" --quit" 'quit' 60) }
    $t0 = Get-Date
    while ((Get-Process -Name wren -ErrorAction SilentlyContinue) -and ((Get-Date) - $t0).TotalSeconds -lt 20) { Start-Sleep -Milliseconds 250 }
    if (Get-Process -Name wren -ErrorAction SilentlyContinue) { throw 'Wren is still running: quit it from the tray icon and run this again' }
}
$code = Invoke-Outside "`"$staged`" /S" 'install' 300
if ($code -ne 0) { throw "the installer exited with code $code" }

# ---- 4. checks ----
if (-not (Test-Path $exe)) { throw "wren.exe is not at $exe after installing" }
$strays = @(Get-PackageCacheStrays | Where-Object { $_ -like '*\Programs\Wren' })
if ($strays.Count -gt 0) { throw "the install landed in the Claude package cache: $($strays -join ', ')" }
if (-not (Test-HasMarker $exe "wren-build-id:$buildId")) { throw "the installed wren.exe is not build $buildId (another build overwrote the shared target folder?)" }
$uninst = Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*' -ErrorAction SilentlyContinue |
    Where-Object { $_.DisplayName -eq 'Wren' } | Select-Object -First 1
$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Wren.lnk'
Write-Host "installed: $exe (build $buildId)"
if ($uninst) { Write-Host "uninstall entry: $($uninst.UninstallString) (version $($uninst.DisplayVersion))" }
Write-Host "Start menu shortcut: $startMenu ($(if (Test-Path $startMenu) { 'present' } else { 'MISSING' }))"

# ---- 5. self-test ----
$selftestCode = $null
if (-not $NoSelftest) {
    Step 'Installed self-test'
    $console = Join-Path $StageDir 'selftest-console.txt'
    $selftestCode = Invoke-Outside "`"$exe`" --selftest > `"$console`" 2>&1" 'selftest' 300
    $kept = Join-Path $dataRoot 'logs\selftest.txt'
    if (Test-Path $kept) { Get-Content $kept | ForEach-Object { Write-Host $_ } } else { Get-Content $console | ForEach-Object { Write-Host $_ } }
    Write-Host "self-test exit code: $selftestCode"
}

# ---- 6. launch ----
if ($wasRunning -or $Launch) {
    Step 'Start Wren in the tray'
    $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = "`"$exe`" --hidden" }
    if ($r.ReturnValue -ne 0) { throw "WMI could not start Wren (code $($r.ReturnValue))" }
    Write-Host "Wren started hidden in the tray (pid $($r.ProcessId))"
}

Step 'Where everything is'
Write-Host "program:        $installDir"
Write-Host "settings:       $settingsRoot\settings.json"
Write-Host "data:           $dataRoot\{db,audio,models,runtime,logs}"
Write-Host "self-test log:  $dataRoot\logs\selftest.txt"
Write-Host "installer:      $staged"
Write-Host "uninstall:      Settings > Apps > Installed apps > Wren (your history and the speech engine stay)"
$left = @(Get-PackageCacheStrays)
if ($left.Count -gt 0) { Write-Host "note: older Wren folders exist in the Claude package cache (not touched): $($left -join ', ')" }
if ($null -ne $selftestCode -and $selftestCode -ne 0) { Write-Host 'RELEASE FAIL: the installed self-test failed'; exit 1 }
Write-Host 'RELEASE OK'
exit 0
