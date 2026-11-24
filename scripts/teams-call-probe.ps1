# Capture Teams meeting-window accessibility dumps during a real call (meetings plan Task 14).
#   powershell -ExecutionPolicy Bypass -File scripts\teams-call-probe.ps1 -Minutes 4
# Writes src-tauri\tests\fixtures\teams\raw\win_call_NNN.json every few seconds (git-ignored: they hold
# real names; redact before turning any into a committed fixture).
param([int]$Minutes = 4, [int]$EverySeconds = 3)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$probe = Join-Path $root "src-tauri\target\debug\examples\teams_probe.exe"
if (-not (Test-Path $probe)) {
  & cargo build --manifest-path (Join-Path $root "src-tauri\Cargo.toml") --example teams_probe
}
$out = Join-Path $root "src-tauri\tests\fixtures\teams\raw"
New-Item -ItemType Directory -Force $out | Out-Null
$end = (Get-Date).AddMinutes($Minutes)
$n = 0
while ((Get-Date) -lt $end) {
  $n++
  $file = Join-Path $out ("win_call_{0:D3}.json" -f $n)
  & $probe --out $file 2>&1 | Out-Null
  if ($LASTEXITCODE -eq 0) { Write-Host ("{0:HH:mm:ss} saved {1}" -f (Get-Date), (Split-Path -Leaf $file)) }
  else { Write-Host ("{0:HH:mm:ss} no meeting window" -f (Get-Date)) }
  Start-Sleep -Seconds $EverySeconds
}
