# Read-only check that this PC can build and run Wren. Changes nothing except writing its report.
# Exit code 0 = all required items present. Writes a report to docs\prereqs-report.txt.
$ErrorActionPreference = 'Continue'
$report = @()
$fail = $false
function Check($name, $ok, $detail, $required = $true) {
    $status = if ($ok) { 'OK  ' } elseif ($required) { 'FAIL' } else { 'WARN' }
    if (-not $ok -and $required) { $script:fail = $true }
    $script:report += ('{0} {1,-22} {2}' -f $status, $name, $detail)
}

$rustc = (Get-Command rustc -ErrorAction SilentlyContinue)
Check 'rustc' ($null -ne $rustc) $(if ($rustc) { (& rustc --version) } else { 'missing' })
$toolchain = try { (& rustup show active-toolchain) -join ' ' } catch { '' }
Check 'msvc toolchain' ($toolchain -match 'msvc') $toolchain
$vsDir = Test-Path 'C:\Program Files (x86)\Microsoft Visual Studio\2022'
Check 'VS 2022 build tools' $vsDir 'C:\Program Files (x86)\Microsoft Visual Studio\2022'
$node = (Get-Command node -ErrorAction SilentlyContinue)
Check 'node' ($null -ne $node) $(if ($node) { (& node --version) } else { 'missing' })
$uv = (Get-Command uv -ErrorAction SilentlyContinue)
Check 'uv' ($null -ne $uv) $(if ($uv) { (& uv --version) } else { 'missing' })
$smi = try { (& nvidia-smi --query-gpu=name,driver_version,memory.total,memory.used --format=csv,noheader) } catch { $null }
Check 'nvidia GPU' ($null -ne $smi) "$smi"
$cudaLine = try { (& nvidia-smi | Select-String 'CUDA').ToString().Trim() } catch { '' }
Check 'CUDA driver >= 12' ($cudaLine -match 'CUDA (UMD )?Version: (1[2-9]|[2-9]\d)') $cudaLine
$drive = Get-PSDrive -Name C
$freeGb = [math]::Round($drive.Free / 1GB, 1)
Check 'free disk >= 15 GB' ($freeGb -ge 15) "$freeGb GB free on C:"
$webview = Test-Path 'C:\Program Files (x86)\Microsoft\EdgeWebView\Application'
Check 'WebView2 runtime' $webview 'EdgeWebView present'
$locked = (Get-Process -Name LogonUI -ErrorAction SilentlyContinue) -ne $null
Check 'session unlocked' (-not $locked) $(if ($locked) { 'LogonUI running (locked)' } else { 'unlocked' }) $false
foreach ($other in 'Willow Voice', 'glaido-core') {
    $p = Get-Process -Name $other -ErrorAction SilentlyContinue
    Check "not running: $other" ($null -eq $p) $(if ($p) { 'RUNNING - would double-paste' } else { 'not running' }) $false
}

$out = Join-Path $PSScriptRoot '..\docs\prereqs-report.txt'
$report | Set-Content -Encoding utf8 $out
$report | ForEach-Object { Write-Output $_ }
if ($fail) { exit 1 } else { exit 0 }
