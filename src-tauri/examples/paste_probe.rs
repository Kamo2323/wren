//! Live check for paste (Task 5): opens scripts/test-page.html as an Edge app window with a
//! throwaway profile, focuses it, pastes three strings with `paste::paste_text`, reads them back
//! from the page title, and checks the clipboard survived (an image put there first must come back
//! byte for byte) and that Wren's text is not in Windows clipboard history.
//!
//!   cargo run --example paste_probe                      # textarea, three strings
//!   cargo run --example paste_probe -- --focus ce        # the chat-style composer
//!   cargo run --example paste_probe -- --focus none      # page with nothing editable focused (observation only:
//!                                                        # Chromium reads the clipboard on Ctrl+V anyway)
//!   cargo run --example paste_probe -- --focus sink      # a plain window that never reads the clipboard:
//!                                                        # expect Ctrl+V, Ctrl+Shift+V, then "not read"
//!   cargo run --example paste_probe -- --keep-open       # leave the Edge window open afterwards
//!
//! Safety: keys are only ever sent by `paste_text`, which first checks that the test page is the
//! foreground window. If the page cannot be brought to the front without sending keys, the probe
//! stops. The user's clipboard is backed up first and put back at the end.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
    SetForegroundWindow, WM_CLOSE,
};
use windows::core::BOOL;
use wren_lib::paste::clipboard::{CF_DIB, SavedFormat, Snapshot, flush};
use wren_lib::paste::{self, ForegroundInfo, PasteResult, REASON_NOT_READ};

const TITLE_PREFIX: &str = "Wren test page | ";

struct Args {
    focus: String,
    keep_open: bool,
}

fn parse_args() -> Args {
    let mut args = Args { focus: "ta".into(), keep_open: false };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--focus" => args.focus = it.next().expect("--focus ta|ce|none"),
            "--keep-open" => args.keep_open = true,
            "--sink" => run_sink(),
            other => panic!("unknown argument {other}"),
        }
    }
    args
}

fn find_edge() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for root in [r"C:\Program Files (x86)\Microsoft\EdgeCore", r"C:\Program Files\Microsoft\EdgeCore"] {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let exe = e.path().join("msedge.exe");
                if exe.exists() {
                    candidates.push(exe);
                }
            }
        }
    }
    for p in [r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"] {
        let p = PathBuf::from(p);
        if p.exists() {
            candidates.push(p);
        }
    }
    candidates.sort();
    candidates.pop()
}

fn test_page() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("scripts").join("test-page.html").canonicalize().expect("scripts/test-page.html exists")
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 4096];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

struct Found(Vec<(HWND, u32)>);

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Found) };
    if unsafe { IsWindowVisible(hwnd) }.as_bool() && window_title(hwnd).starts_with(TITLE_PREFIX) {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        found.0.push((hwnd, pid));
    }
    BOOL(1)
}

fn find_test_windows() -> Vec<(HWND, u32)> {
    let mut found = Found(Vec::new());
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(&mut found as *mut Found as isize)) };
    found.0
}

#[derive(Debug, Default, serde::Deserialize)]
struct PageState {
    ta: String,
    ce: String,
    n: u32,
    /// Focused element id while the window has focus ("" otherwise).
    #[serde(default)]
    f: String,
    /// The page's last few log lines.
    #[serde(default)]
    log: Vec<String>,
}

fn page_state(hwnd: HWND) -> Option<PageState> {
    let title = window_title(hwnd);
    serde_json::from_str(title.strip_prefix(TITLE_PREFIX)?).ok()
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// A 32×32 32-bit DIB with a gradient (BITMAPINFOHEADER + pixels).
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
            v.extend_from_slice(&[(x * 8) as u8, (y * 8) as u8, 128, 255]);
        }
    }
    v
}

/// Searches Windows clipboard history for `needle` via WinRT from Windows PowerShell. Prints
/// nothing of the history itself; returns a one-line verdict.
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
  $hits = 0; $texts = 0; $new = 0
  $since = [DateTimeOffset]::Parse('{since_utc}')
  foreach ($item in $r.Items) {{
    if ($item.Timestamp -ge $since) {{ $new++ }}
    if ($item.Content.Contains('Text')) {{
      $texts++
      $t = Await ($item.Content.GetTextAsync()) ([string])
      if ($t -and $t.Contains('{needle}')) {{ $hits++ }}
    }}
  }}
  "ITEMS $(@($r.Items).Count) TEXT $texts NEW $new HITS $hits"
}} catch {{ "ERROR $($_.Exception.Message)" }}
"#
    );
    match Command::new("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output() {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Err(e) => format!("ERROR cannot run powershell: {e}"),
    }
}

fn main() {
    let args = parse_args();
    let mut failures: Vec<String> = Vec::new();

    if !find_test_windows().is_empty() {
        eprintln!("A test page window is already open; close it first.");
        std::process::exit(2);
    }
    let edge = find_edge().expect("msedge.exe not found under EdgeCore or Edge\\Application");
    let page = test_page();
    let page_url = format!(
        "file:///{}?focus={}",
        page.to_string_lossy().trim_start_matches(r"\\?\").replace('\\', "/"),
        args.focus
    );
    let profile = std::env::temp_dir().join(format!("wren-paste-probe-edge-{}", std::process::id()));
    println!("edge: {}\npage: {page_url}", edge.display());

    let since_utc = chrono::Utc::now().to_rfc3339();
    // 1. The user's clipboard, so it can be put back whatever happens.
    let user_clipboard = Snapshot::capture().expect("back up the clipboard");
    println!("user clipboard backed up ({} formats)", user_clipboard.formats.len());

    let mut child = if args.focus == "sink" {
        // Another process, so paste_text does not refuse it as Wren's own window.
        Command::new(std::env::current_exe().unwrap()).arg("--sink").spawn().expect("start the sink window")
    } else {
        Command::new(&edge)
            .arg(format!("--app={page_url}"))
            .arg(format!("--user-data-dir={}", profile.display()))
            .args(["--no-first-run", "--no-default-browser-check", "--new-window"])
            .spawn()
            .expect("start Edge")
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&args, &mut failures, &since_utc)));
    let window = match &result {
        Ok(w) => *w,
        Err(_) => {
            failures.push("probe panicked".into());
            None
        }
    };

    // Put the user's clipboard back and close the window we opened.
    flush();
    match user_clipboard.restore() {
        Ok(()) => println!("user clipboard restored"),
        Err(e) => failures.push(format!("could not restore the user's clipboard: {e}")),
    }
    let after = clipboard_history_check("wren-no-such-text", &since_utc);
    println!("clipboard history after restoring the user's clipboard: {after}");
    if after.contains("NEW") && !after.contains("NEW 0 ") {
        failures.push(format!("restoring added an entry to clipboard history ({after})"));
    }
    if !args.keep_open {
        for (hwnd, _) in find_test_windows() {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        let _ = window;
        std::thread::sleep(Duration::from_millis(1500));
        let _ = child.kill();
        let _ = std::fs::remove_dir_all(&profile);
    }

    if failures.is_empty() {
        println!("PASTE PROBE PASS");
    } else {
        for f in &failures {
            println!("PASTE PROBE FAIL: {f}");
        }
        std::process::exit(1);
    }
}

fn run(args: &Args, failures: &mut Vec<String>, since_utc: &str) -> Option<isize> {
    // 2. Wait for the page window.
    let start = Instant::now();
    let (hwnd, pid) = loop {
        if let Some(&w) = find_test_windows().first() {
            break w;
        }
        if start.elapsed() > Duration::from_secs(20) {
            failures.push("the test page window did not appear within 20 s".into());
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let target = ForegroundInfo { hwnd: hwnd.0 as isize, pid, process_name: paste::process_name(pid), window_title: String::new() };
    println!("test page window {:#x} pid {pid} ({})", target.hwnd, target.process_name);
    // Let the page finish loading and focus its box.
    std::thread::sleep(Duration::from_millis(800));

    // 3. Make sure it is in front — without sending any keys.
    let start = Instant::now();
    while unsafe { GetForegroundWindow() } != hwnd {
        if start.elapsed() < Duration::from_millis(500) {
            let _ = unsafe { SetForegroundWindow(hwnd) };
        } else {
            // Windows' foreground lock (active for a while after any input) blocks a background
            // process; sharing the foreground thread's input state for a moment lifts it. No keys.
            bring_to_front_attached(hwnd);
        }
        if start.elapsed() > Duration::from_secs(3) {
            failures.push(format!("could not bring the test page to the front (foreground is {})", paste::foreground().process_name));
            return Some(target.hwnd);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("test page is the foreground window");
    // The window being in front is not enough: Chromium reads the clipboard on Ctrl+V even before
    // its text box has keyboard focus, and then inserts nothing. Wait for the page to say which
    // element has focus.
    let want_focus = match args.focus.as_str() {
        "ta" => Some("ta"),
        "ce" => Some("ce"),
        "none" => Some("log"),
        _ => None, // the sink has no page
    };
    if let Some(want) = want_focus {
        let start = Instant::now();
        loop {
            let f = page_state(hwnd).map(|s| s.f).unwrap_or_default();
            if f == want {
                println!("page reports focus on #{f} after {} ms", start.elapsed().as_millis());
                break;
            }
            if start.elapsed() > Duration::from_secs(3) {
                failures.push(format!("the page did not report focus on #{want} (has {f:?})"));
                return Some(target.hwnd);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Even then, for a few seconds after a forced activation Chromium can route keys to its
        // frame instead of the page (and still read the clipboard on Ctrl+V). Tap Shift — no text,
        // tagged — until the page logs it, so the pastes below test Wren rather than Chromium focus.
        let start = Instant::now();
        let mut taps = 0;
        loop {
            if unsafe { GetForegroundWindow() } != hwnd {
                failures.push("the test page lost the foreground before pasting".into());
                return Some(target.hwnd);
            }
            wren_lib::hotkey::send_tagged_keys(&[(0x10, true), (0x10, false)]);
            taps += 1;
            std::thread::sleep(Duration::from_millis(150));
            if page_state(hwnd).is_some_and(|s| s.log.iter().any(|l| l.ends_with("key Shift"))) {
                println!("page receives keys after {} ms ({taps} Shift taps)", start.elapsed().as_millis());
                break;
            }
            if start.elapsed() > Duration::from_secs(10) {
                failures.push("the page never received a key".into());
                return Some(target.hwnd);
            }
        }
    }

    // 4. An image (and nothing else) on the clipboard.
    let image = test_dib();
    Snapshot { formats: vec![SavedFormat { format: CF_DIB, data: image.clone() }] }.restore().expect("put the test image on the clipboard");
    let image_hash = hash(&image);
    println!("test image on clipboard: CF_DIB {} bytes, hash {image_hash:016x}", image.len());

    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let nonce = format!("{:08x}", std::process::id() ^ nanos);
    let strings = [
        format!("Hello from the Wren probe {nonce}."),
        " Second, with commas — ünïcödé and “quotes”.".to_string(),
        " Third: 日本語 ✓ done".to_string(),
    ];
    let field = |s: &PageState| if args.focus == "ce" { s.ce.clone() } else { s.ta.clone() };

    // 5. Paste each string and read the page back.
    let mut expected = String::new();
    let mut latencies = Vec::new();
    for (i, s) in strings.iter().enumerate() {
        let started = Instant::now();
        let r = paste::paste_text(s, &target, &|_| false);
        let took = started.elapsed().as_millis();
        let receipt = paste::last_receipt();
        if args.focus == "none" {
            // Observation only: does a page with no editable focus still read the clipboard?
            let landed = page_state(hwnd).map(|st| st.ta.len() + st.ce.len()).unwrap_or(0);
            println!(
                "paste {}: {:?} in {took} ms, receipt {:?}; text landed on the page: {} (OBSERVATION: a read receipt proves a read, not an insertion)",
                i + 1,
                r,
                receipt,
                if landed > 0 { "yes" } else { "no" }
            );
            break;
        }
        if args.focus == "sink" {
            let ok = r == PasteResult::Failed(REASON_NOT_READ.into());
            println!("paste {}: {:?} in {took} ms (expected not read after Ctrl+V and Ctrl+Shift+V) -> {}", i + 1, r, if ok { "ok" } else { "UNEXPECTED" });
            if !ok {
                failures.push(format!("paste {} into the sink returned {r:?}", i + 1));
            }
            if wren_lib::paste::clipboard::read_text().as_deref() == Some(s.as_str()) {
                println!("  text left on the clipboard: yes");
            } else {
                failures.push(format!("paste {}: the unread text was not left on the clipboard", i + 1));
            }
            break; // one is enough: each takes 2 s of waiting
        }
        if r != PasteResult::Pasted {
            failures.push(format!("paste {} returned {r:?}", i + 1));
            break;
        }
        expected.push_str(s);
        let receipt = receipt.expect("a receipt after Pasted");
        latencies.push(receipt.latency_ms);
        // The page echoes into its title; give it a moment.
        let wait = Instant::now();
        let mut got = String::new();
        while wait.elapsed() < Duration::from_secs(2) {
            if let Some(state) = page_state(hwnd) {
                got = field(&state);
                if got == expected {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let ok = got == expected;
        println!(
            "paste {}: {:?} via {:?}, read receipt {} ms after the keys, paste_text returned in {took} ms -> page {}",
            i + 1,
            r,
            receipt.chord,
            receipt.latency_ms,
            if ok { "matches" } else { "MISMATCH" }
        );
        if !ok {
            failures.push(format!("paste {}: page has {got:?}, expected {expected:?}", i + 1));
            if let Some(state) = page_state(hwnd) {
                for l in state.log {
                    println!("    page log: {l}");
                }
            }
        }
        // Let the restore happen before the next paste (as a real second dictation would).
        flush();
    }
    if let Some(state) = page_state(hwnd) {
        println!("page paste events: {}", state.n);
    }
    if !latencies.is_empty() {
        println!("read-receipt latency ms: {latencies:?}");
    }

    // 6. The image must be back, byte for byte (the unread case leaves the text instead, by design).
    flush();
    if args.focus != "none" && args.focus != "sink" {
        let now = Snapshot::capture().expect("read the clipboard");
        match now.get(CF_DIB) {
            Some(bytes) if hash(bytes) == image_hash => println!("image survived: hash {:016x} matches", hash(bytes)),
            Some(bytes) => failures.push(format!("image changed: hash {:016x} != {image_hash:016x}", hash(bytes))),
            None => failures.push("image gone from the clipboard".into()),
        }
    }

    // 7. Wren's text must not be in clipboard history.
    let verdict = clipboard_history_check(&nonce, since_utc);
    println!("clipboard history check (NEW = entries added since the probe started): {verdict}");
    if verdict.contains("HITS") && !verdict.ends_with("HITS 0") {
        failures.push(format!("Wren's text is in clipboard history ({verdict})"));
    }
    if verdict.contains("NEW") && !verdict.contains("NEW 0 ") {
        failures.push(format!("the probe added entries to clipboard history ({verdict})"));
    }
    Some(target.hwnd)
}

fn bring_to_front_attached(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::BringWindowToTop;
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        let attached = fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(me, fg_thread, false);
        }
    }
}

/// `--sink`: a plain visible window titled like the test page that ignores every key, so nothing
/// ever reads the clipboard. Exits when closed.
fn run_sink() -> ! {
    use windows::Win32::Foundation::LRESULT;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostQuitMessage,
        RegisterClassW, WINDOW_EX_STYLE, WM_DESTROY, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };
    use windows::core::{HSTRING, w};

    unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_DESTROY {
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }
    unsafe {
        let instance = GetModuleHandleW(None).unwrap().into();
        let class = w!("WrenProbeSink");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance, lpszClassName: class, ..Default::default() });
        let title = HSTRING::from(format!("{TITLE_PREFIX}{{\"ta\":\"\",\"ce\":\"\",\"n\":0,\"f\":\"\"}}"));
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            &title,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            420,
            200,
            None,
            None,
            Some(instance),
            None,
        )
        .unwrap();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
    }
    std::process::exit(0)
}
