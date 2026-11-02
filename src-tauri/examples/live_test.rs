//! Live end-to-end test of the whole app (Task 8, spec §13). Run by `scripts/live-test.ps1`, which
//! refuses first if the session is locked or Willow/Glaido is running.
//!
//!   cargo run --example live_test -- --wren <wren.exe> --home <temp dir> --report docs/live-test-report.md
//!
//! What it does:
//! 1. Starts the real Wren (`--hidden`) with `WREN_HOME` = a temp folder, `WREN_RUNTIME_ROOT` = the
//!    installed engine, and `WREN_TEST_AUDIO` = a speech fixture (the microphone is replaced by it).
//! 2. Opens scripts/test-page.html in an Edge app window with a throwaway profile and brings it to the
//!    front without sending keys.
//! 3. Injects the real chord (untagged, so Wren's hook takes it as typed): N holds of 3 s into the
//!    textarea, a hands-free double-tap run, then a hold into the chat-style composer. Checks the page
//!    received the expected words, that the foreground window was the test page the whole time while
//!    Wren's popup was showing, that an image on the clipboard survives, and the release-to-paste time.
//! 4. 50 chord taps: Start / Search must never come to the front. Ctrl+Win+Right: all three keys reach
//!    Windows, Wren discards and pastes nothing.
//! 5. Cleans up (closes the page, stops Wren, puts the user's clipboard back), checks Windows clipboard
//!    history gained nothing and Wren's logs hold no dictated words, writes the report.
//!
//! Safety: before every injected key the foreground window must be the test page (never claude.exe);
//! otherwise the test stops. Never uses Notepad; never starts Willow or Glaido.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetForegroundWindow, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    PostMessageW, SetForegroundWindow, WM_CLOSE, WS_EX_NOACTIVATE,
};
use windows::core::BOOL;
use wren_lib::constants::{CLIPBOARD_RESTORE_MS, DOUBLE_TAP_GAP_MS, HANDSFREE_STOP_DEBOUNCE_MS, VK_CHORD_CTRL, VK_CHORD_WIN};
use wren_lib::hotkey::{send_keys_with_extra_info, send_tagged_keys};
use wren_lib::paste::clipboard::{CF_DIB, SavedFormat, Snapshot, flush};
use wren_lib::paste::{foreground, process_name};

const TITLE_PREFIX: &str = "Wren test page | ";
const VK_RIGHT: u16 = 0x27;
const VK_LEFT: u16 = 0x25;
const VK_ESCAPE: u16 = 0x1B;
const VK_SHIFT: u16 = 0x10;
const START_HOSTS: [&str; 2] = ["startmenuexperiencehost.exe", "searchhost.exe"];
const HOLD_MS: u64 = 3_000;
/// Words of weather.wav ("The weather in London is grey and drizzly again this morning.") said
/// inside the first 3 s.
const HOLD_EXPECT: [&str; 2] = ["weather", "london"];
const FULL_EXPECT: [&str; 3] = ["weather", "london", "morning"];
/// Words of the fixture that must never appear in Wren's logs ("weather" is left out: the test-seam
/// warning names the file `weather.wav`).
const PRIVATE_WORDS: [&str; 4] = ["london", "drizzly", "morning", "grey and"];

struct Args {
    wren: PathBuf,
    home: PathBuf,
    report: PathBuf,
    holds: usize,
    taps: usize,
    /// Only the Task 9 rounds (watcher in the textarea and the composer, pre-paste check).
    task9: bool,
}

fn parse_args() -> Args {
    let mut a = Args { wren: PathBuf::new(), home: PathBuf::new(), report: PathBuf::from("docs/live-test-report.md"), holds: 10, taps: 50, task9: false };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| panic!("{arg} needs a value"));
        match arg.as_str() {
            "--wren" => a.wren = PathBuf::from(val()),
            "--home" => a.home = PathBuf::from(val()),
            "--report" => a.report = PathBuf::from(val()),
            "--holds" => a.holds = val().parse().expect("--holds N"),
            "--taps" => a.taps = val().parse().expect("--taps N"),
            "--task9" => a.task9 = true,
            other => panic!("unknown argument {other}"),
        }
    }
    assert!(a.wren.is_file(), "--wren must name wren.exe");
    assert!(!a.home.as_os_str().is_empty(), "--home must name a temp folder");
    a
}

// ---------- windows, processes, page ----------

fn window_title(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 32_768];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn top_windows() -> Vec<HWND> {
    unsafe extern "system" fn proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe { (*(lparam.0 as *mut Vec<HWND>)).push(hwnd) };
        BOOL(1)
    }
    let mut v: Vec<HWND> = Vec::new();
    let _ = unsafe { EnumWindows(Some(proc), LPARAM(&mut v as *mut Vec<HWND> as isize)) };
    v
}

fn test_windows() -> Vec<HWND> {
    top_windows()
        .into_iter()
        .filter(|h| unsafe { IsWindowVisible(*h) }.as_bool() && window_title(*h).starts_with(TITLE_PREFIX))
        .collect()
}

/// Wren's popup: a top-level window of the Wren process with WS_EX_NOACTIVATE.
fn popup_window(wren_pid: u32) -> Option<HWND> {
    top_windows().into_iter().find(|h| {
        window_pid(*h) == wren_pid && unsafe { GetWindowLongPtrW(*h, GWL_EXSTYLE) } & WS_EX_NOACTIVATE.0 as isize != 0
    })
}

#[derive(Debug, Default, Clone, serde::Deserialize)]
struct PageState {
    ta: String,
    ce: String,
    /// Pastes the page received.
    #[serde(default)]
    n: u32,
    #[serde(default)]
    f: String,
    #[serde(default)]
    log: Vec<String>,
    /// Viewport rects (CSS px) of the boxes, and the device pixel ratio.
    #[serde(default)]
    rect: std::collections::HashMap<String, [f64; 4]>,
    #[serde(default)]
    dpr: f64,
}

fn page_state(hwnd: HWND) -> Option<PageState> {
    serde_json::from_str(window_title(hwnd).strip_prefix(TITLE_PREFIX)?).ok()
}

fn fg_exe() -> String {
    foreground().process_name
}

fn rivals_running() -> Vec<String> {
    let out = Command::new("tasklist").args(["/FO", "CSV", "/NH"]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
    let mut found = Vec::new();
    for (needle, label) in [("willow", "Willow"), ("glaido", "Glaido")] {
        if out.contains(needle) {
            found.push(label.to_string());
        }
    }
    found
}

fn process_running(exe: &str) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("IMAGENAME eq {exe}"), "/FO", "CSV", "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase().contains(&exe.to_lowercase()))
        .unwrap_or(false)
}

fn find_edge() -> Option<PathBuf> {
    let mut c: Vec<PathBuf> = Vec::new();
    for root in [r"C:\Program Files (x86)\Microsoft\EdgeCore", r"C:\Program Files\Microsoft\EdgeCore"] {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let exe = e.path().join("msedge.exe");
                if exe.exists() {
                    c.push(exe);
                }
            }
        }
    }
    for p in [r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"] {
        if Path::new(p).exists() {
            c.push(PathBuf::from(p));
        }
    }
    c.sort();
    c.pop()
}

/// Brings `hwnd` to the front without sending any key: shares the foreground thread's input state
/// for a moment (lifts the foreground lock), and on alternate tries uses `SwitchToThisWindow`, which
/// gets past some dialogs (e.g. a stray "Pick an application") that refuse `SetForegroundWindow`.
fn bring_to_front_attached(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{BringWindowToTop, SW_RESTORE, ShowWindow, SwitchToThisWindow};
    static TRY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if TRY.fetch_add(1, Ordering::Relaxed) % 2 == 1 {
            SwitchToThisWindow(hwnd, true);
        }
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

/// The page's render widget (its client area is the web viewport).
fn render_widget(page: HWND) -> Option<HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW};
    unsafe extern "system" fn proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        if String::from_utf16_lossy(&buf[..n.max(0) as usize]) == "Chrome_RenderWidgetHostHWND" {
            unsafe { *(lparam.0 as *mut Option<HWND>) = Some(hwnd) };
            return BOOL(0);
        }
        BOOL(1)
    }
    let mut found: Option<HWND> = None;
    let _ = unsafe { EnumChildWindows(Some(page), Some(proc), LPARAM(&mut found as *mut Option<HWND> as isize)) };
    found
}

/// Clicks the middle of the page's `which` box ("ta" or "ce"). Windows always lets a click activate
/// a window, even when a stray dialog holds the foreground lock; the click lands only on the test
/// page (checked with WindowFromPoint) and focuses the box the test wants anyway. The cursor is put
/// back afterwards.
fn click_box(page: HWND, which: &str) -> Result<(), String> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput};
    use windows::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, GetCursorPos, SetCursorPos, WindowFromPoint};
    let st = page_state(page).ok_or("cannot read the page")?;
    let r = st.rect.get(which).ok_or("the page does not report its boxes")?;
    let widget = render_widget(page).ok_or("no render widget")?;
    let dpr = if st.dpr > 0.0 { st.dpr } else { 1.0 };
    let mut pt = POINT { x: ((r[0] + r[2] / 2.0) * dpr) as i32, y: ((r[1] + (r[3] / 2.0).min(18.0)) * dpr) as i32 };
    unsafe {
        if !ClientToScreen(widget, &mut pt).as_bool() {
            return Err("ClientToScreen failed".into());
        }
        let under = GetAncestor(WindowFromPoint(pt), GA_ROOT);
        if under != page {
            return Err(format!("another window covers the test page at ({}, {})", pt.x, pt.y));
        }
        let mut old = POINT::default();
        let _ = GetCursorPos(&mut old);
        let _ = SetCursorPos(pt.x, pt.y);
        std::thread::sleep(Duration::from_millis(30));
        let mi = |flags| INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT { dwFlags: flags, ..Default::default() } } };
        SendInput(&[mi(MOUSEEVENTF_LEFTDOWN), mi(MOUSEEVENTF_LEFTUP)], size_of::<INPUT>() as i32);
        std::thread::sleep(Duration::from_millis(30));
        let _ = SetCursorPos(old.x, old.y);
    }
    Ok(())
}

struct Page {
    hwnd: HWND,
    child: Child,
    profile: PathBuf,
}

impl Page {
    fn close(mut self) {
        let _ = unsafe { PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        std::thread::sleep(Duration::from_millis(1200));
        let _ = self.child.kill();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
    fn state(&self) -> PageState {
        page_state(self.hwnd).unwrap_or_default()
    }
}

/// Opens the test page with `focus` (ta | ce), brings it to the front with no keys, waits until it
/// reports focus on that element and receives a (tagged, text-free) Shift.
fn open_page(edge: &Path, home: &Path, focus: &str) -> Result<Page, String> {
    let page = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("scripts").join("test-page.html").canonicalize().map_err(|e| e.to_string())?;
    let url = format!("file:///{}?focus={focus}", page.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/"));
    let profile = home.join(format!("edge-{focus}"));
    let child = Command::new(edge)
        .arg(format!("--app={url}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--no-first-run", "--no-default-browser-check", "--new-window"])
        .spawn()
        .map_err(|e| format!("cannot start Edge: {e}"))?;
    let t = Instant::now();
    let hwnd = loop {
        if let Some(&h) = test_windows().first() {
            break h;
        }
        if t.elapsed() > Duration::from_secs(20) {
            return Err("the test page did not appear within 20 s".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let p = Page { hwnd, child, profile };
    std::thread::sleep(Duration::from_millis(800));
    let t = Instant::now();
    let mut clicks = 0;
    while unsafe { GetForegroundWindow() } != hwnd {
        if t.elapsed() < Duration::from_millis(500) {
            let _ = unsafe { SetForegroundWindow(hwnd) };
        } else if t.elapsed() < Duration::from_millis(2_000) || clicks >= 3 {
            bring_to_front_attached(hwnd);
        } else {
            // Something holds the foreground lock: click the box itself (no keys).
            clicks += 1;
            bring_to_front_attached(hwnd);
            match click_box(hwnd, focus) {
                Ok(()) => println!("  focused the test page with a click on #{focus} (Windows refused SetForegroundWindow)"),
                Err(e) => println!("  click to focus not possible: {e}"),
            }
            std::thread::sleep(Duration::from_millis(400));
        }
        if t.elapsed() > Duration::from_secs(10) {
            let fg = fg_exe();
            p.close();
            return Err(format!("could not bring the test page to the front (foreground is {fg})"));
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    // ?focus=none puts focus on the (non-editable) log.
    let want = if focus == "none" { "log" } else { focus };
    let t = Instant::now();
    while p.state().f != want {
        if t.elapsed() > Duration::from_secs(3) {
            let f = p.state().f;
            p.close();
            return Err(format!("the page did not report focus on #{want} (has {f:?})"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // After a forced activation Chromium can route keys to its frame for a few seconds: tap a
    // tagged Shift (no text; Wren ignores tagged keys) until the page logs it.
    let t = Instant::now();
    loop {
        if unsafe { GetForegroundWindow() } != hwnd {
            p.close();
            return Err("the test page lost the foreground".into());
        }
        send_tagged_keys(&[(VK_SHIFT, true), (VK_SHIFT, false)]);
        std::thread::sleep(Duration::from_millis(150));
        if p.state().log.iter().any(|l| l.ends_with("key Shift")) {
            break;
        }
        if t.elapsed() > Duration::from_secs(10) {
            p.close();
            return Err("the page never received a key".into());
        }
    }
    Ok(p)
}

// ---------- keys (always guarded) ----------

/// Every injected key goes through here: the test page must be in front, never claude.exe.
fn guarded(page: HWND, keys: &[(u16, bool)]) -> Result<(), String> {
    let fg = unsafe { GetForegroundWindow() };
    if fg != page {
        return Err(format!("foreground is {} (not the test page); stopped before sending keys", fg_exe()));
    }
    if fg_exe() == "claude.exe" {
        return Err("claude.exe is in front; stopped".into());
    }
    send_keys_with_extra_info(keys, 0);
    Ok(())
}

/// Releases are always sent (stuck Ctrl/Win would be worse), but never while claude.exe is in front.
fn release_chord() {
    if !async_down(VK_CHORD_WIN) && !async_down(VK_CHORD_CTRL) {
        return;
    }
    if fg_exe() == "claude.exe" {
        eprintln!("claude.exe is in front: not sending the key-ups (press and release Ctrl and Win once if they feel stuck)");
        return;
    }
    send_keys_with_extra_info(&[(VK_CHORD_WIN, false)], 0);
    std::thread::sleep(Duration::from_millis(10));
    send_keys_with_extra_info(&[(VK_CHORD_CTRL, false)], 0);
}

fn chord_down(page: HWND) -> Result<(), String> {
    guarded(page, &[(VK_CHORD_CTRL, true)])?;
    std::thread::sleep(Duration::from_millis(15));
    if let Err(e) = guarded(page, &[(VK_CHORD_WIN, true)]) {
        send_keys_with_extra_info(&[(VK_CHORD_CTRL, false)], 0);
        return Err(e);
    }
    Ok(())
}

fn chord_tap(page: HWND, hold_ms: u64, variant: usize) -> Result<(), String> {
    let (first, second) = if variant.is_multiple_of(2) { (VK_CHORD_CTRL, VK_CHORD_WIN) } else { (VK_CHORD_WIN, VK_CHORD_CTRL) };
    guarded(page, &[(first, true)])?;
    std::thread::sleep(Duration::from_millis(15));
    if let Err(e) = guarded(page, &[(second, true)]) {
        send_keys_with_extra_info(&[(first, false)], 0);
        return Err(e);
    }
    std::thread::sleep(Duration::from_millis(hold_ms));
    if (variant / 2).is_multiple_of(2) {
        release_chord();
    } else {
        send_keys_with_extra_info(&[(VK_CHORD_CTRL, false)], 0);
        std::thread::sleep(Duration::from_millis(10));
        send_keys_with_extra_info(&[(VK_CHORD_WIN, false)], 0);
    }
    Ok(())
}

fn async_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

// ---------- observation ----------

/// Samples the foreground window and the popup's visibility every 5 ms until stopped.
struct Sampler {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<(Vec<isize>, bool, usize)>,
}

impl Sampler {
    fn start(popup: Option<HWND>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let popup = popup.map(|h| h.0 as isize);
        let handle = std::thread::spawn(move || {
            let (mut fgs, mut seen, mut n) = (Vec::<isize>::new(), false, 0usize);
            while !s.load(Ordering::Relaxed) {
                let fg = unsafe { GetForegroundWindow() }.0 as isize;
                if !fgs.contains(&fg) {
                    fgs.push(fg);
                }
                if let Some(p) = popup {
                    seen |= unsafe { IsWindowVisible(HWND(p as *mut _)) }.as_bool();
                }
                n += 1;
                std::thread::sleep(Duration::from_millis(5));
            }
            (fgs, seen, n)
        });
        Self { stop, handle }
    }
    /// (every foreground window seen, popup was visible at some point, samples)
    fn finish(self) -> (Vec<isize>, bool, usize) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap_or_default()
    }
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

fn has_all(text: &str, want: &[&str]) -> bool {
    let w = words(text);
    want.iter().all(|x| w.iter().any(|y| y == x))
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

fn test_dib() -> Vec<u8> {
    let (w, h) = (32i32, 32i32);
    let mut v = Vec::new();
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    v.extend_from_slice(&[0u8; 16]);
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[(x * 8) as u8, (y * 8) as u8, 200, 255]);
        }
    }
    v
}

/// Lines of Wren's JSON logs: (message, fields).
fn wren_log(home: &Path) -> Vec<(String, serde_json::Value)> {
    let dir = home.join("local").join("logs");
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("wren"))).collect();
    files.sort();
    for f in files {
        for line in std::fs::read_to_string(&f).unwrap_or_default().lines() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                let fields = v.get("fields").cloned().unwrap_or_default();
                let msg = fields.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
                out.push((msg, fields));
            }
        }
    }
    out
}

fn log_count(home: &Path, msg: &str) -> usize {
    wren_log(home).iter().filter(|(m, _)| m == msg).count()
}

/// Searches Windows clipboard history (WinRT, Windows PowerShell). One-line verdict, no content.
fn clipboard_history_check(needle: &str, since_utc: &str) -> String {
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
try {{
  Add-Type -AssemblyName System.Runtime.WindowsRuntime
  $null = [Windows.ApplicationModel.DataTransfer.Clipboard, Windows.ApplicationModel.DataTransfer, ContentType = WindowsRuntime]
  $asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{ $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' }})[0]
  function Await($op, [Type]$t) {{ $task = $asTask.MakeGenericMethod($t).Invoke($null, @($op)); $null = $task.Wait(5000); $task.Result }}
  if (-not [Windows.ApplicationModel.DataTransfer.Clipboard]::IsHistoryEnabled()) {{ 'HISTORY-DISABLED'; exit }}
  $r = Await ([Windows.ApplicationModel.DataTransfer.Clipboard]::GetHistoryItemsAsync()) ([Windows.ApplicationModel.DataTransfer.ClipboardHistoryItemsResult])
  if ($r.Status -ne 'Success') {{ "STATUS-$($r.Status)"; exit }}
  $hits = 0; $new = 0
  $since = [DateTimeOffset]::Parse('{since_utc}')
  foreach ($item in $r.Items) {{
    if ($item.Timestamp -ge $since) {{ $new++ }}
    if ($item.Content.Contains('Text')) {{
      $t = Await ($item.Content.GetTextAsync()) ([string])
      if ($t -and $t.ToLower().Contains('{needle}')) {{ $hits++ }}
    }}
  }}
  "ITEMS $(@($r.Items).Count) NEW $new HITS $hits"
}} catch {{ "ERROR $($_.Exception.Message)" }}
"#
    );
    match Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => format!("ERROR {e}"),
    }
}

fn percentile(v: &[u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    let mut s = v.to_vec();
    s.sort_unstable();
    s[(((s.len() - 1) as f64) * p).round() as usize]
}

// ---------- the run ----------

#[derive(Default)]
struct Dictation {
    mode: &'static str,
    landed: bool,
    fg_ok: bool,
    popup_seen: bool,
    samples: usize,
    page_ms: Option<u64>,
    words: usize,
    newline: bool,
    note: String,
}

#[derive(Default)]
struct Results {
    ready_ms: u64,
    dictations: Vec<Dictation>,
    image: Option<bool>,
    start_opened: Option<(usize, usize)>,
    taps_pasted_nothing: Option<bool>,
    passthrough: Option<(bool, String)>,
    history: String,
    log_private: Option<bool>,
    release_to_paste: Vec<u64>,
    hf_release_to_paste: Vec<u64>,
    failures: Vec<String>,
    notes: Vec<String>,
}

/// One dictation into the page. `hands_free` double-taps, waits for the clip, taps to stop.
fn dictate(page: &Page, popup: Option<HWND>, field: &str, hands_free: bool, expect: &[&str]) -> Result<Dictation, String> {
    let before = page.state();
    let get = |s: &PageState| if field == "ce" { s.ce.clone() } else { s.ta.clone() };
    let before_text = get(&before);
    let sampler = Sampler::start(popup);
    let released;
    if hands_free {
        chord_tap(page.hwnd, 120, 0)?;
        std::thread::sleep(Duration::from_millis(200));
        chord_tap(page.hwnd, 120, 0)?;
        // weather.wav is 4.4 s; let it play out, well past the stop debounce.
        std::thread::sleep(Duration::from_millis(6_000.max(HANDSFREE_STOP_DEBOUNCE_MS + 100)));
        chord_down(page.hwnd)?;
        released = Instant::now();
        std::thread::sleep(Duration::from_millis(120));
        release_chord();
    } else {
        chord_down(page.hwnd)?;
        std::thread::sleep(Duration::from_millis(HOLD_MS));
        release_chord();
        released = Instant::now();
    }
    let mut after_text = before_text.clone();
    let mut page_ms = None;
    while released.elapsed() < Duration::from_secs(15) {
        let now = get(&page.state());
        if now != before_text && now.len() > before_text.len() {
            // Give the page a moment to finish echoing, then take the final value.
            std::thread::sleep(Duration::from_millis(60));
            after_text = get(&page.state());
            page_ms = Some(released.elapsed().as_millis() as u64 - 60);
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Leave time for the clipboard restore and the popup to hide.
    std::thread::sleep(Duration::from_millis(CLIPBOARD_RESTORE_MS + 900));
    let (fgs, popup_seen, samples) = sampler.finish();
    let delta = after_text.strip_prefix(&before_text).unwrap_or(&after_text).to_string();
    let sent = page.state().log.iter().any(|l| l.contains("SEND"));
    Ok(Dictation {
        mode: if hands_free { "hands-free" } else { "hold" },
        landed: page_ms.is_some() && has_all(&delta, expect),
        fg_ok: fgs == vec![page.hwnd.0 as isize],
        popup_seen,
        samples,
        page_ms,
        words: words(&delta).len(),
        newline: delta.contains('\n') || sent,
        note: if fgs.len() > 1 { format!("{} different foreground windows seen", fgs.len()) } else { String::new() },
    })
}

fn run(args: &Args, r: &mut Results, wren: &mut Option<Child>) -> Result<(), String> {
    let edge = find_edge().ok_or("msedge.exe not found")?;
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
    let audio = fixtures.join("weather.wav");
    let runtime_root = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("no LOCALAPPDATA")?).join("Wren");

    // 1. Wren, hidden, with the fake microphone.
    println!("starting Wren: {}", args.wren.display());
    let t0 = Instant::now();
    let child = Command::new(&args.wren)
        .arg("--hidden")
        .env("WREN_HOME", &args.home)
        .env("WREN_RUNTIME_ROOT", &runtime_root)
        .env("WREN_TEST_AUDIO", &audio)
        .spawn()
        .map_err(|e| format!("cannot start Wren: {e}"))?;
    let wren_pid = child.id();
    *wren = Some(child);
    loop {
        let ready = wren_log(&args.home).iter().any(|(m, f)| m == "engine status" && f.get("ready") == Some(&serde_json::Value::Bool(true)));
        if ready {
            break;
        }
        if t0.elapsed() > Duration::from_secs(90) {
            return Err("Wren's engine was not ready within 90 s".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    r.ready_ms = t0.elapsed().as_millis() as u64;
    println!("Wren ready in {} ms (pid {wren_pid})", r.ready_ms);
    let popup = popup_window(wren_pid);
    if popup.is_none() {
        r.notes.push("Wren's popup window was not found, so popup visibility was not sampled".into());
    }

    if args.task9 {
        return run_task9(args, &edge, popup, r);
    }

    // 2. Hold dictations into the textarea.
    let page = open_page(&edge, &args.home, "ta")?;
    println!("test page (textarea) in front: {:#x} ({})", page.hwnd.0 as isize, process_name(window_pid(page.hwnd)));
    let image = test_dib();
    Snapshot { formats: vec![SavedFormat { format: CF_DIB, data: image.clone() }] }.restore().map_err(|e| format!("cannot put the test image on the clipboard: {e}"))?;
    for i in 0..args.holds {
        let d = dictate(&page, popup, "ta", false, &HOLD_EXPECT)?;
        println!(
            "hold {}: landed {} | foreground unchanged {} ({} samples) | popup seen {} | page saw text {:?} ms after release | {} words",
            i + 1,
            d.landed,
            d.fg_ok,
            d.samples,
            d.popup_seen,
            d.page_ms,
            d.words
        );
        if i == 0 {
            flush();
            let now = Snapshot::capture().map_err(|e| e.to_string())?;
            let ok = now.get(CF_DIB).is_some_and(|b| hash(b) == hash(&image));
            println!("image on the clipboard survived the dictation: {ok}");
            r.image = Some(ok);
        }
        r.dictations.push(d);
        std::thread::sleep(Duration::from_millis(DOUBLE_TAP_GAP_MS + 200));
    }

    // 3. Hands-free.
    let d = dictate(&page, popup, "ta", true, &FULL_EXPECT)?;
    println!("hands-free: landed {} | foreground unchanged {} | popup seen {} | page saw text {:?} ms after the stop press", d.landed, d.fg_ok, d.popup_seen, d.page_ms);
    r.dictations.push(d);
    std::thread::sleep(Duration::from_millis(DOUBLE_TAP_GAP_MS + 200));

    // 4. Start menu: taps (discarded) with every press order.
    let ta_before = page.state().ta;
    let discards_before = log_count(&args.home, "recording discarded");
    let mut opened = 0;
    for i in 0..args.taps {
        chord_tap(page.hwnd, 150, i)?;
        let t = Instant::now();
        let mut start = None;
        while t.elapsed() < Duration::from_millis(700) {
            let exe = fg_exe();
            if START_HOSTS.contains(&exe.as_str()) {
                start = Some(exe);
                break;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if let Some(exe) = start {
            opened += 1;
            println!("  tap {i}: {exe} came to the front");
            for _ in 0..5 {
                std::thread::sleep(Duration::from_millis(500));
                if !START_HOSTS.contains(&fg_exe().as_str()) {
                    break;
                }
                send_tagged_keys(&[(VK_ESCAPE, true), (VK_ESCAPE, false)]);
            }
            bring_to_front_attached(page.hwnd);
            std::thread::sleep(Duration::from_millis(400));
        }
        std::thread::sleep(Duration::from_millis(DOUBLE_TAP_GAP_MS + 150));
    }
    std::thread::sleep(Duration::from_millis(1_000));
    let discards = log_count(&args.home, "recording discarded") - discards_before;
    println!("START_OPENED {opened}/{}  (Wren discarded {discards} taps)", args.taps);
    r.start_opened = Some((opened, args.taps));
    r.taps_pasted_nothing = Some(page.state().ta == ta_before);

    // 5. Passthrough: Ctrl+Win+Right reaches Windows; Wren cancels and pastes nothing.
    let discards_before = log_count(&args.home, "recording discarded");
    guarded(page.hwnd, &[(VK_CHORD_CTRL, true)])?;
    std::thread::sleep(Duration::from_millis(15));
    guarded(page.hwnd, &[(VK_CHORD_WIN, true)])?;
    std::thread::sleep(Duration::from_millis(40));
    let arrow_sent = guarded(page.hwnd, &[(VK_RIGHT, true)]);
    std::thread::sleep(Duration::from_millis(30));
    let reached = arrow_sent.is_ok() && [VK_CHORD_CTRL, VK_CHORD_WIN, VK_RIGHT].iter().all(|vk| async_down(*vk));
    send_keys_with_extra_info(&[(VK_RIGHT, false)], 0);
    std::thread::sleep(Duration::from_millis(20));
    release_chord();
    arrow_sent?;
    std::thread::sleep(Duration::from_millis(1_200));
    let after_right = fg_exe();
    let mut moved_back = String::new();
    if unsafe { GetForegroundWindow() } != page.hwnd {
        // Another virtual desktop came up: go back, only if claude.exe is not in front.
        if after_right != "claude.exe" {
            send_keys_with_extra_info(&[(VK_CHORD_CTRL, true), (VK_CHORD_WIN, true), (VK_LEFT, true), (VK_LEFT, false)], 0);
            release_chord();
            std::thread::sleep(Duration::from_millis(1_200));
            moved_back = format!("; moved back with Ctrl+Win+Left, foreground now {}", fg_exe());
        } else {
            return Err("Ctrl+Win+Right brought claude.exe to the front; stopped (no keys sent to it)".into());
        }
    }
    std::thread::sleep(Duration::from_millis(1_500));
    let discarded = log_count(&args.home, "recording discarded") - discards_before;
    let nothing = page.state().ta == ta_before;
    let pass = reached && discarded >= 1 && nothing;
    let detail = format!("all three keys showed as down to Windows: {reached}; Wren discarded the chord: {}; nothing pasted: {nothing}; foreground after: {after_right}{moved_back}", discarded >= 1);
    println!("PASSTHROUGH {}: {detail}", if pass { "OK" } else { "FAIL" });
    r.passthrough = Some((pass, detail));
    page.close();

    // 6. Chat-style composer.
    std::thread::sleep(Duration::from_millis(500));
    let page = open_page(&edge, &args.home, "ce")?;
    let d = dictate(&page, popup, "ce", false, &HOLD_EXPECT)?;
    println!("composer: landed {} | foreground unchanged {} | popup seen {} | page saw text {:?} ms | Enter/newline {}", d.landed, d.fg_ok, d.popup_seen, d.page_ms, d.newline);
    r.dictations.push(Dictation { mode: "hold (composer)", ..d });
    page.close();
    Ok(())
}

fn main() {
    let args = parse_args();
    let rivals = rivals_running();
    if !rivals.is_empty() {
        println!("SKIPPED: {} running (would also react to the chord)", rivals.join(" and "));
        std::process::exit(3);
    }
    if process_running("LogonUI.exe") {
        println!("SKIPPED: the session is locked");
        std::process::exit(3);
    }
    if process_running("wren.exe") {
        println!("SKIPPED: another Wren is running (the single-instance hand-off would swallow the test)");
        std::process::exit(3);
    }
    if !test_windows().is_empty() {
        println!("SKIPPED: a test page window is already open");
        std::process::exit(3);
    }
    let _ = std::fs::remove_dir_all(&args.home);
    std::fs::create_dir_all(&args.home).expect("create the temp home");

    let since_utc = chrono::Utc::now().to_rfc3339();
    let user_clipboard = Snapshot::capture().expect("back up the clipboard");
    println!("user clipboard backed up ({} formats)", user_clipboard.formats.len());

    let mut r = Results::default();
    let mut wren: Option<Child> = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&args, &mut r, &mut wren)));
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => r.failures.push(e),
        Err(_) => r.failures.push("the live test panicked".into()),
    }
    // Clean up whatever happened (a page left open by an early stop is closed here).
    release_chord();
    for h in test_windows() {
        let _ = unsafe { PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)) };
    }
    std::thread::sleep(Duration::from_millis(800));
    if let Some(mut w) = wren.take() {
        let _ = w.kill();
        let _ = w.wait();
    }
    flush();
    match user_clipboard.restore() {
        Ok(()) => println!("user clipboard restored"),
        Err(e) => r.failures.push(format!("could not restore the user's clipboard: {e}")),
    }
    r.history = clipboard_history_check("london", &since_utc);
    println!("clipboard history: {}", r.history);

    // Logs: stage timings, and no dictated words.
    let log = wren_log(&args.home);
    let raw_logs: String = std::fs::read_dir(args.home.join("local").join("logs"))
        .map(|d| d.flatten().map(|e| std::fs::read_to_string(e.path()).unwrap_or_default()).collect::<Vec<_>>().join("\n"))
        .unwrap_or_default()
        .to_lowercase();
    r.log_private = Some(!PRIVATE_WORDS.iter().any(|w| raw_logs.contains(w)));
    for (m, f) in &log {
        if m == "dictation done"
            && let Some(ms) = f.get("release_to_paste_ms").and_then(|v| v.as_u64())
        {
            if f.get("mode").and_then(|v| v.as_str()) == Some("HandsFree") {
                r.hf_release_to_paste.push(ms);
            } else {
                r.release_to_paste.push(ms);
            }
        }
    }

    // Verdicts.
    for (i, d) in r.dictations.iter().enumerate() {
        if !d.landed {
            r.failures.push(format!("dictation {} ({}) did not land on the page", i + 1, d.mode));
        }
        if !d.fg_ok {
            r.failures.push(format!("dictation {} ({}): foreground changed ({})", i + 1, d.mode, d.note));
        }
        if d.newline {
            r.failures.push(format!("dictation {} ({}): a newline or Enter reached the page", i + 1, d.mode));
        }
    }
    if r.image == Some(false) {
        r.failures.push("the image on the clipboard did not survive".into());
    }
    if let Some((k, n)) = r.start_opened
        && k > 0
    {
        r.failures.push(format!("Start opened {k}/{n} times"));
    }
    if r.taps_pasted_nothing == Some(false) {
        r.failures.push("a tap pasted text".into());
    }
    if let Some((false, d)) = &r.passthrough {
        r.failures.push(format!("passthrough: {d}"));
    }
    if r.history.contains("HITS") && !r.history.ends_with("HITS 0") {
        r.failures.push(format!("dictated text is in clipboard history ({})", r.history));
    }
    if r.history.contains("NEW ") && !r.history.contains("NEW 0 ") {
        r.failures.push(format!("clipboard history gained entries ({})", r.history));
    }
    if r.log_private == Some(false) {
        r.failures.push("Wren's logs contain dictated words".into());
    }

    let report = render(&args, &r);
    if let Some(dir) = args.report.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&args.report, report).expect("write the report");
    println!("report: {}", args.report.display());
    if r.failures.is_empty() {
        println!("LIVE TEST PASS");
    } else {
        for f in &r.failures {
            println!("LIVE TEST FAIL: {f}");
        }
        std::process::exit(1);
    }
}

fn yn(b: bool) -> &'static str {
    if b { "yes" } else { "**no**" }
}

fn render(args: &Args, r: &Results) -> String {
    let mut s = String::new();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M %Z");
    s.push_str("# Wren live test report\n\n");
    s.push_str(&format!(
        "Generated by `scripts/live-test.ps1` (`src-tauri/examples/live_test.rs`) on {now}.\n\n\
         **Verdict: {}**\n\n",
        if r.failures.is_empty() { "PASS" } else { "FAIL" }
    ));
    s.push_str(
        "Setup: the real Wren build started hidden with a temporary `WREN_HOME`, the installed speech engine \
         (`WREN_RUNTIME_ROOT`) and `WREN_TEST_AUDIO` = `tests/fixtures/weather.wav` (Hazel en-GB: \"The weather in London \
         is grey and drizzly again this morning.\") in place of the microphone. Target: `scripts/test-page.html` in an Edge \
         app window with a throwaway profile. The chord was injected as untagged Left Ctrl + Left Win key events, only while \
         the test page was the foreground window. The user's clipboard was backed up first and restored at the end.\n\n",
    );
    s.push_str(&format!("Engine ready {} ms after Wren started.\n\n", r.ready_ms));
    s.push_str("## Dictations\n\n| # | Mode | Paste landed | Foreground unchanged | Popup seen | Page had text after (ms) | Words | Newline/Enter |\n|---|---|---|---|---|---|---|---|\n");
    for (i, d) in r.dictations.iter().enumerate() {
        s.push_str(&format!(
            "| {} | {} | {} | {} ({} samples) | {} | {} | {} | {} |\n",
            i + 1,
            d.mode,
            yn(d.landed),
            yn(d.fg_ok),
            d.samples,
            if d.popup_seen { "yes" } else { "no" },
            d.page_ms.map(|m| m.to_string()).unwrap_or_else(|| "—".into()),
            d.words,
            if d.newline { "**yes**" } else { "no" },
        ));
    }
    let holds: Vec<u64> = r.dictations.iter().filter(|d| d.mode == "hold").filter_map(|d| d.page_ms).collect();
    s.push_str(&format!(
        "\nPer mode — hold (textarea): {}/{} landed; hands-free: {}; hold into the chat composer: {}.\n\n",
        r.dictations.iter().filter(|d| d.mode == "hold" && d.landed).count(),
        r.dictations.iter().filter(|d| d.mode == "hold").count(),
        r.dictations.iter().find(|d| d.mode == "hands-free").map(|d| if d.landed { "landed" } else { "**did not land**" }).unwrap_or("not run"),
        r.dictations.iter().find(|d| d.mode == "hold (composer)").map(|d| if d.landed { "landed" } else { "**did not land**" }).unwrap_or("not run"),
    ));
    s.push_str("## Release-to-paste latency\n\n");
    s.push_str(&format!(
        "- Wren's own measure (chord release → target read the clipboard; `release_to_paste_ms` in the dictation log), \
         holds (textarea and composer), n = {}: **p50 {} ms, p95 {} ms** (min {}, max {}).\n",
        r.release_to_paste.len(),
        percentile(&r.release_to_paste, 0.5),
        percentile(&r.release_to_paste, 0.95),
        r.release_to_paste.iter().min().copied().unwrap_or(0),
        r.release_to_paste.iter().max().copied().unwrap_or(0),
    ));
    if !r.hf_release_to_paste.is_empty() {
        s.push_str(&format!("- Hands-free (stop press → read): {} ms.\n", r.hf_release_to_paste[0]));
    }
    s.push_str(&format!(
        "- Seen from the page (release → text visible in the textarea, polled every 10 ms), hold, n = {}: p50 {} ms, p95 {} ms.\n\n",
        holds.len(),
        percentile(&holds, 0.5),
        percentile(&holds, 0.95)
    ));
    s.push_str("## Checks\n\n| Check | Result |\n|---|---|\n");
    s.push_str(&format!(
        "| Image on the clipboard survives a dictation (byte for byte) | {} |\n",
        r.image.map(yn).unwrap_or("not run")
    ));
    if let Some((k, n)) = r.start_opened {
        s.push_str(&format!("| Start menu never opens over chord taps (every press/release order) | START_OPENED {k}/{n} |\n"));
    }
    s.push_str(&format!("| Taps (< 400 ms) paste nothing | {} |\n", r.taps_pasted_nothing.map(yn).unwrap_or("not run")));
    if let Some((pass, d)) = &r.passthrough {
        s.push_str(&format!("| Ctrl+Win+Right passes through | {} — {d} |\n", if *pass { "pass" } else { "**fail**" }));
    }
    s.push_str(&format!("| Windows clipboard history after the run (`NEW` = entries added; `HITS` = dictated text found) | {} |\n", r.history));
    s.push_str(&format!("| Wren's logs contain no dictated words | {} |\n", r.log_private.map(yn).unwrap_or("not run")));
    s.push_str(&format!("\nForeground sampling: every 5 ms from the chord press until 1.4 s after the paste; \"unchanged\" means the only foreground window seen was the test page while Wren's popup showed and hid.\n\nArguments: {} holds, {} taps.\n", args.holds, args.taps));
    if !r.notes.is_empty() {
        s.push_str("\n## Notes\n\n");
        for n in &r.notes {
            s.push_str(&format!("- {n}\n"));
        }
    }
    if !r.failures.is_empty() {
        s.push_str("\n## Failures\n\n");
        for f in &r.failures {
            s.push_str(&format!("- {f}\n"));
        }
    }
    s.push_str(
        "\n## Known limit\n\nA read receipt proves the target app read the clipboard, not that a text box had focus \
         (Task 5). Here the page itself confirms the text arrived; in real use Task 9's watcher reads the focused element.\n",
    );
    s
}

// ---------- Task 9: correction watcher, pre-paste check (`--task9`) ----------

const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
/// The fixture's one uncommon word (not in the bundled common-words list), so its fix becomes an
/// active replacement at once, with the toast.
const T9_FROM: &str = "drizzly";
const T9_TO: &str = "drizzle";

fn tap(page: HWND, vk: u16) -> Result<(), String> {
    guarded(page, &[(vk, true), (vk, false)])?;
    std::thread::sleep(Duration::from_millis(20));
    Ok(())
}

/// Replaces the last `from` in the box with `to` (lower-case a–z) the way a person would: arrow
/// keys to the end of the word, Backspace over it, type the new one.
fn edit_word(page: &Page, field: &str, from: &str, to: &str) -> Result<String, String> {
    let get = |s: &PageState| if field == "ce" { s.ce.clone() } else { s.ta.clone() };
    let text = get(&page.state());
    let lower = text.to_lowercase();
    let pos = lower.rfind(from).ok_or_else(|| format!("{from:?} is not in the {field} box (the engine heard something else)"))?;
    let after = text[pos + from.len()..].chars().count();
    for _ in 0..after {
        tap(page.hwnd, VK_LEFT)?;
    }
    for _ in 0..from.chars().count() {
        tap(page.hwnd, VK_BACK)?;
    }
    for c in to.chars() {
        tap(page.hwnd, c.to_ascii_uppercase() as u16)?;
    }
    std::thread::sleep(Duration::from_millis(200));
    let now = get(&page.state());
    let want = format!("{}{to}{}", &text[..pos], &text[pos + from.len()..]);
    if now != want {
        return Err(format!("the edit did not come out as expected in the {field} box"));
    }
    Ok(now)
}

/// Saves the screen area of `hwnd` as a 32-bit BMP (to look at the popup's toast afterwards).
fn grab_window(hwnd: HWND, path: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
        DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut r = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut r) }.map_err(|e| e.to_string())?;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    if w <= 0 || h <= 0 {
        return Err("empty popup rectangle".into());
    }
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let _ = BitBlt(mem, 0, 0, w, h, Some(screen), r.left, r.top, SRCCOPY);
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        GetDIBits(mem, bmp, 0, h as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    }
    let mut f = Vec::with_capacity(54 + pixels.len());
    f.extend_from_slice(b"BM");
    f.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
    f.extend_from_slice(&[0; 4]);
    f.extend_from_slice(&54u32.to_le_bytes());
    f.extend_from_slice(&40u32.to_le_bytes());
    f.extend_from_slice(&w.to_le_bytes());
    f.extend_from_slice(&(-h).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    f.extend_from_slice(&[0; 24]);
    f.extend_from_slice(&pixels);
    std::fs::write(path, f).map_err(|e| e.to_string())
}

fn db_path(home: &Path) -> PathBuf {
    home.join("local").join("db").join("wren.db")
}

/// Log lines with `msg` written after the first `skip` of them.
fn log_since(home: &Path, msg: &str, skip: usize) -> Vec<serde_json::Value> {
    wren_log(home).into_iter().filter(|(m, _)| m == msg).skip(skip).map(|(_, f)| f).collect()
}

/// One watcher round: dictate (hands-free, the whole fixture) into `field`, fix `drizzly`, end the
/// watch with `end_key` (Tab moves focus; Enter "sends" the composer), then check the log, the
/// popup and the database.
fn watcher_round(args: &Args, edge: &Path, popup: Option<HWND>, field: &str, end_key: u16, r: &mut Results) -> Result<(), String> {
    let page = open_page(edge, &args.home, field)?;
    let learned_before = log_count(&args.home, "learned from a correction");
    let ended_before = log_count(&args.home, "watch ended");
    let checks_before = log_count(&args.home, "pre-paste check");
    let d = dictate(&page, popup, field, true, &FULL_EXPECT)?;
    println!("[task9 {field}] dictation landed {} ({} words, page saw it {:?} ms after the stop press)", d.landed, d.words, d.page_ms);
    let check = log_since(&args.home, "pre-paste check", checks_before);
    println!("[task9 {field}] pre-paste check: {:?}", check.iter().map(|f| (f.get("decision").cloned(), f.get("control").cloned(), f.get("ms").cloned())).collect::<Vec<_>>());
    if !d.landed {
        page.close();
        return Err(format!("[task9 {field}] the dictation did not land"));
    }
    let t_edit = Instant::now();
    edit_word(&page, field, T9_FROM, T9_TO)?;
    println!("[task9 {field}] fixed {T9_FROM} -> {T9_TO} with arrow keys, Backspace and typing ({} ms)", t_edit.elapsed().as_millis());
    // Let the watcher see the change, then end the watch.
    std::thread::sleep(Duration::from_millis(1_500));
    let sampler = Sampler::start(popup);
    let t_end = Instant::now();
    tap(page.hwnd, end_key)?;
    let mut learned = None;
    while t_end.elapsed() < Duration::from_secs(6) {
        if let Some(f) = log_since(&args.home, "learned from a correction", learned_before).into_iter().next() {
            learned = Some((f, t_end.elapsed().as_millis() as u64));
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(700));
    let grab = args.home.join(format!("toast-{field}.bmp"));
    let grabbed = popup.map(|p| grab_window(p, &grab));
    let (_, popup_seen, _) = sampler.finish();
    let ended = log_since(&args.home, "watch ended", ended_before);
    println!(
        "[task9 {field}] watch ended: {:?}",
        ended.iter().map(|f| (f.get("reason").cloned(), f.get("reads").cloned(), f.get("watched_ms").cloned())).collect::<Vec<_>>()
    );
    match &learned {
        Some((f, ms)) => println!("[task9 {field}] learned {ms} ms after the end key: decisions {} entry ids {}", f["decisions"], f["entry_ids"]),
        None => r.failures.push(format!("[task9 {field}] nothing was learned within 6 s")),
    }
    println!("[task9 {field}] popup visible after learning: {popup_seen}; toast grab: {grabbed:?} -> {}", grab.display());
    if !popup_seen {
        r.failures.push(format!("[task9 {field}] the learned toast was not seen"));
    }
    let store = wren_lib::store::Store::open(&db_path(&args.home)).map_err(|e| e.to_string())?;
    let entries = store.dictionary_list().map_err(|e| e.to_string())?;
    let rep = entries
        .iter()
        .find(|e| e.from_text.as_deref() == Some(T9_FROM) && e.to_text == T9_TO && e.kind == wren_lib::dictionary::EntryKind::Replacement);
    match rep {
        Some(e) => println!("[task9 {field}] dictionary: replacement #{} {T9_FROM} -> {T9_TO} status {:?} source {:?}", e.id, e.status, e.source),
        None => r.failures.push(format!("[task9 {field}] no {T9_FROM} -> {T9_TO} replacement in the database")),
    }
    if let Some(e) = rep
        && (e.status != wren_lib::dictionary::Status::Active || e.source != wren_lib::dictionary::Source::LearnedPaste)
    {
        r.failures.push(format!("[task9 {field}] the replacement is {:?}/{:?}, not active/learned_paste", e.status, e.source));
    }
    let term = entries.iter().any(|e| e.kind == wren_lib::dictionary::EntryKind::Term && e.to_text == T9_TO);
    println!("[task9 {field}] dictionary: term {T9_TO} present {term}");
    // Clear what was learned, so the next round can learn it again from scratch.
    for e in entries.iter().filter(|e| e.to_text == T9_TO) {
        let _ = store.delete_entry(e.id);
    }
    page.close();
    r.notes.push(format!("task9 {field}: learned {}, popup seen {popup_seen}", learned.is_some()));
    Ok(())
}

/// The pre-paste check on a page where nothing editable has focus: nothing reaches the page, the
/// text is copied, the popup says "Not pasted".
fn prepaste_round(args: &Args, edge: &Path, popup: Option<HWND>, r: &mut Results) -> Result<(), String> {
    let page = open_page(edge, &args.home, "none")?;
    let checks_before = log_count(&args.home, "pre-paste check");
    let done_before = log_count(&args.home, "dictation done");
    let sampler = Sampler::start(popup);
    chord_down(page.hwnd)?;
    std::thread::sleep(Duration::from_millis(HOLD_MS));
    release_chord();
    let t = Instant::now();
    let mut done = None;
    while t.elapsed() < Duration::from_secs(15) {
        if let Some(f) = log_since(&args.home, "dictation done", done_before).into_iter().next() {
            done = Some(f);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(1_000));
    let (_, popup_seen, _) = sampler.finish();
    let st = page.state();
    let check = log_since(&args.home, "pre-paste check", checks_before);
    println!(
        "[task9 none] pre-paste check: {:?}",
        check.iter().map(|f| (f.get("decision").cloned(), f.get("control").cloned(), f.get("writable_value").cloned(), f.get("ms").cloned())).collect::<Vec<_>>()
    );
    let outcome = done.as_ref().and_then(|f| f.get("outcome").cloned());
    println!("[task9 none] outcome {outcome:?}; page pastes {} ta {:?} ce {:?}; popup seen {popup_seen}", st.n, st.ta.len(), st.ce.len());
    if !check.iter().any(|f| f.get("decision").and_then(|d| d.as_str()) == Some("NotEditable")) {
        r.failures.push("[task9 none] the pre-paste check did not hold the paste back".into());
    }
    if st.n != 0 || !st.ta.is_empty() || !st.ce.is_empty() {
        r.failures.push("[task9 none] something reached the page".into());
    }
    if outcome.as_ref().and_then(|o| o.as_str()) != Some("NotPasted") {
        r.failures.push(format!("[task9 none] outcome {outcome:?}, expected NotPasted"));
    }
    page.close();
    Ok(())
}

fn run_task9(args: &Args, edge: &Path, popup: Option<HWND>, r: &mut Results) -> Result<(), String> {
    watcher_round(args, edge, popup, "ta", VK_TAB, r)?;
    std::thread::sleep(Duration::from_millis(500));
    watcher_round(args, edge, popup, "ce", VK_RETURN, r)?;
    std::thread::sleep(Duration::from_millis(500));
    prepaste_round(args, edge, popup, r)?;
    Ok(())
}
