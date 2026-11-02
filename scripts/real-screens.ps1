<#
.SYNOPSIS
  Screenshots of the real, installed Wren's main window (Task 10): Home, History, Dictionary, Stats
  and Settings, saved to docs/screenshots/real/.

.DESCRIPTION
  Starts the given wren.exe (default: the installed one) with WREN_HOME = a live-test home, so the
  pages show the test dictations from the test WAVs and never Maximilian's own history, and with
  WREN_TEST_AUDIO set so the microphone is never opened. Pages are switched through WebView2's
  DevTools port (scripts/lib/cdp-nav.mjs): no keys or clicks are sent to Windows. Each page is
  captured with PrintWindow (PW_RENDERFULLCONTENT) of the Wren window. Wren is then asked to quit.

  Run scripts/live-test.ps1 -Wren <exe> -WorkDir <dir> first so <dir>\home holds dictations.

.EXAMPLE
  powershell -ExecutionPolicy Bypass -File scripts\real-screens.ps1 -TestHome C:\Wren-wt\wren-live-test\home
#>
param(
    [Parameter(Mandatory = $true)][string]$TestHome,
    [string]$Wren = (Join-Path $env:LOCALAPPDATA 'Programs\Wren\wren.exe'),
    [string]$Out = '',
    [int]$Port = 9333
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Out) { $Out = Join-Path $repo 'docs\screenshots\real' }
New-Item -ItemType Directory -Force $Out | Out-Null
if (Get-Process -Name wren -ErrorAction SilentlyContinue) { 'SKIPPED: another Wren is running'; exit 3 }
if (-not (Test-Path (Join-Path $TestHome 'local\db\wren.db'))) { throw "no test database in $TestHome (run the live test first)" }

Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
using System.Text;
public static class WrenGrab {
    [DllImport("user32.dll")] static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern int GetWindowLong(IntPtr h, int idx);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    delegate bool EnumProc(IntPtr h, IntPtr l);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    // The visible top-level window of `pid` without WS_EX_NOACTIVATE (that one is the popup).
    public static IntPtr MainWindow(uint pid) {
        IntPtr found = IntPtr.Zero;
        EnumWindows((h, l) => {
            uint p; GetWindowThreadProcessId(h, out p);
            if (p == pid && IsWindowVisible(h) && (GetWindowLong(h, -20) & 0x08000000) == 0) {
                var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
                if (sb.ToString().EndsWith("Wren")) { found = h; return false; }
            }
            return true;
        }, IntPtr.Zero);
        return found;
    }
    public static string Title(IntPtr h) { var sb = new StringBuilder(256); GetWindowText(h, sb, 256); return sb.ToString(); }
    public static void Save(IntPtr h, string path) {
        RECT r; GetWindowRect(h, out r);
        int w = r.R - r.L, ht = r.B - r.T;
        using (var bmp = new Bitmap(w, ht, PixelFormat.Format32bppArgb)) {
            using (var g = Graphics.FromImage(bmp)) {
                IntPtr hdc = g.GetHdc();
                PrintWindow(h, hdc, 2); // PW_RENDERFULLCONTENT: WebView2 content too
                g.ReleaseHdc(hdc);
            }
            // Crop the invisible resize border: keep the DWM frame bounds only.
            RECT f;
            Bitmap outBmp = bmp;
            if (DwmGetWindowAttribute(h, 9, out f, Marshal.SizeOf(typeof(RECT))) == 0) {
                var crop = new Rectangle(f.L - r.L, f.T - r.T, f.R - f.L, f.B - f.T);
                if (crop.Width > 0 && crop.Height > 0 && crop.Right <= w && crop.Bottom <= ht) outBmp = bmp.Clone(crop, bmp.PixelFormat);
            }
            outBmp.Save(path, ImageFormat.Png);
            if (outBmp != bmp) outBmp.Dispose();
        }
    }
}
'@ -ReferencedAssemblies System.Drawing

$runtimeRoot = Join-Path $env:LOCALAPPDATA 'Wren'
$psi = New-Object System.Diagnostics.ProcessStartInfo $Wren
$psi.UseShellExecute = $false
$psi.EnvironmentVariables['WREN_HOME'] = $TestHome
$psi.EnvironmentVariables['WREN_RUNTIME_ROOT'] = $runtimeRoot
$psi.EnvironmentVariables['WREN_TEST_AUDIO'] = Join-Path $repo 'src-tauri\tests\fixtures\weather.wav'
$psi.EnvironmentVariables['WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS'] = "--remote-debugging-port=$Port"
$proc = [System.Diagnostics.Process]::Start($psi)
"started $Wren (pid $($proc.Id)) with WREN_HOME=$TestHome"
try {
    $hwnd = [IntPtr]::Zero
    $t0 = Get-Date
    while ($hwnd -eq [IntPtr]::Zero) {
        if (((Get-Date) - $t0).TotalSeconds -gt 60) { throw 'the Wren window did not appear within 60 s' }
        Start-Sleep -Milliseconds 300
        $hwnd = [WrenGrab]::MainWindow([uint32]$proc.Id)
    }
    # Let the engine come up so the sidebar shows it ready, and the first page settle.
    Start-Sleep -Seconds 12
    $shots = @(
        @('home', 'top', 'home.png'),
        @('history', 'top', 'history.png'),
        @('dictionary', 'top', 'dictionary.png'),
        @('stats', 'top', 'stats.png'),
        @('settings', 'top', 'settings.png'),
        @('settings', 'bottom', 'settings-bottom.png')
    )
    foreach ($s in $shots) {
        & node (Join-Path $repo 'scripts\lib\cdp-nav.mjs') $Port $s[0] $s[1]
        if ($LASTEXITCODE -ne 0) { throw "could not switch to $($s[0])" }
        $file = Join-Path $Out $s[2]
        [WrenGrab]::Save($hwnd, $file)
        "saved $file ($([WrenGrab]::Title($hwnd)))"
    }
} finally {
    $q = New-Object System.Diagnostics.ProcessStartInfo -ArgumentList $Wren, '--quit'
    $q.UseShellExecute = $false
    $q.EnvironmentVariables['WREN_HOME'] = $TestHome
    $q.EnvironmentVariables['WREN_RUNTIME_ROOT'] = $runtimeRoot
    [System.Diagnostics.Process]::Start($q).WaitForExit(20000) | Out-Null
    if (-not $proc.WaitForExit(20000)) { $proc.Kill(); 'Wren did not quit in 20 s; stopped it' } else { 'Wren quit' }
}
