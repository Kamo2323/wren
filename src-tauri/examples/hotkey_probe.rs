//! Live check tool for the Ctrl+Win hotkey (Task 2).
//!
//!   cargo run --example hotkey_probe                         # print events for 30 s (press Ctrl+Win yourself)
//!   cargo run --example hotkey_probe -- --seconds 60
//!   cargo run --example hotkey_probe -- --selftest-start-menu 50
//!   cargo run --example hotkey_probe -- --passthrough
//!   cargo run --example hotkey_probe -- --selftest-modes
//!
//! Self-tests inject *untagged* keys (so Wren's hook reacts as if they were typed) and refuse to
//! run unless the foreground window belongs to `--require-foreground` (default `notepad.exe`).
//! They are skipped if Willow or Glaido is running, because those apps would react to the chord.
//! `--no-service` runs the same injections without Wren (a control: does Start open without the
//! mask?). `--control-win-tap` injects one plain Win tap to prove Start detection works.

use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;
use wren_lib::constants::{DOUBLE_TAP_GAP_MS, HANDSFREE_STOP_DEBOUNCE_MS, TAP_MAX_MS, VK_CHORD_CTRL, VK_CHORD_WIN};
use wren_lib::hotkey::{HotkeyEvent, HotkeyService, chord_keys_physically_up, send_keys_with_extra_info, send_tagged_keys};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};
use windows::core::PWSTR;

const VK_RIGHT: u16 = 0x27;
const VK_LEFT: u16 = 0x25;
const VK_ESCAPE: u16 = 0x1B;
/// Unassigned key (0xE8) sent *untagged* as "some other key" during a hold.
const VK_OTHER: u16 = 0xE8;
const START_HOSTS: [&str; 2] = ["startmenuexperiencehost.exe", "searchhost.exe"];

struct Args {
    seconds: u64,
    start_menu: Option<usize>,
    passthrough: bool,
    modes: bool,
    no_service: bool,
    control_win_tap: bool,
    require_fg: String,
}

fn parse_args() -> Args {
    let mut a = Args {
        seconds: 30,
        start_menu: None,
        passthrough: false,
        modes: false,
        no_service: false,
        control_win_tap: false,
        require_fg: "notepad.exe".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seconds" => a.seconds = it.next().and_then(|v| v.parse().ok()).unwrap_or(30),
            "--selftest-start-menu" => a.start_menu = Some(it.next().and_then(|v| v.parse().ok()).unwrap_or(50)),
            "--passthrough" => a.passthrough = true,
            "--selftest-modes" => a.modes = true,
            "--no-service" => a.no_service = true,
            "--control-win-tap" => a.control_win_tap = true,
            "--require-foreground" => a.require_fg = it.next().unwrap_or_default().to_lowercase(),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

fn main() {
    let args = parse_args();
    let selftest = args.start_menu.is_some() || args.passthrough || args.modes || args.control_win_tap;

    if selftest {
        let rivals = rival_dictation_apps();
        if !rivals.is_empty() {
            println!("SKIPPED: {} running (they react to Ctrl+Win); not touching them", rivals.join(", "));
            return;
        }
    }

    let (tx, rx) = crossbeam_channel::unbounded();
    let svc = if args.no_service {
        println!("control run: Wren's hook is NOT installed");
        None
    } else {
        Some(HotkeyService::start(tx).expect("install hotkey hook"))
    };

    if !selftest {
        println!("listening for {} s; press Ctrl+Win (hold, tap, double-tap)…", args.seconds);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(args.seconds) {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(100)) {
                println!("{:>7} ms  {ev:?}", t0.elapsed().as_millis());
            }
        }
        return;
    }

    if !chord_keys_physically_up() {
        println!("ABORT: Ctrl or Win is physically held");
        std::process::exit(1);
    }
    let fg = foreground_exe();
    if fg != args.require_fg || fg == "claude.exe" {
        println!("ABORT: foreground is {fg:?}, need {:?}", args.require_fg);
        std::process::exit(1);
    }
    let home = GetFg::now();
    let mut failed = false;

    if args.control_win_tap {
        drain(&rx);
        send(&[(VK_CHORD_WIN, true)]);
        std::thread::sleep(Duration::from_millis(60));
        send(&[(VK_CHORD_WIN, false)]);
        let seen = watch_for_start(Duration::from_millis(1_200));
        println!("CONTROL_WIN_TAP start_detected={}", seen.is_some());
        if seen.is_some() {
            close_start(&home);
        }
        std::thread::sleep(Duration::from_millis(700));
    }

    if let Some(n) = args.start_menu {
        failed |= !start_menu_selftest(n, &rx, &args, &home, svc.as_ref());
    }
    if args.modes {
        failed |= !modes_selftest(&rx, &args, &home, svc.as_ref());
    }
    if args.passthrough {
        failed |= !passthrough_selftest(&rx, &args);
    }

    if !chord_keys_physically_up() {
        // Never leave a key down.
        send(&[(VK_CHORD_WIN, false), (VK_CHORD_CTRL, false)]);
    }
    if let Some(s) = &svc {
        println!("reinstalls during run: {}", s.reinstall_count());
    }
    drop(svc);
    std::process::exit(if failed { 1 } else { 0 });
}

/// Untagged injection: Wren's hook treats these like real key presses.
fn send(keys: &[(u16, bool)]) {
    send_keys_with_extra_info(keys, 0);
}

fn drain(rx: &Receiver<HotkeyEvent>) -> Vec<HotkeyEvent> {
    std::thread::sleep(Duration::from_millis(30));
    rx.try_iter().collect()
}

/// One chord press held for `hold_ms`. `variant` picks the press/release order and whether both
/// downs go in a single `SendInput` call.
fn chord(variant: usize, hold_ms: u64) {
    let (first, second) = if variant.is_multiple_of(2) { (VK_CHORD_CTRL, VK_CHORD_WIN) } else { (VK_CHORD_WIN, VK_CHORD_CTRL) };
    if variant % 5 == 4 {
        send(&[(first, true), (second, true)]);
    } else {
        send(&[(first, true)]);
        std::thread::sleep(Duration::from_millis(15));
        send(&[(second, true)]);
    }
    std::thread::sleep(Duration::from_millis(hold_ms));
    if (variant / 2).is_multiple_of(2) {
        send(&[(VK_CHORD_WIN, false)]);
        std::thread::sleep(Duration::from_millis(10));
        send(&[(VK_CHORD_CTRL, false)]);
    } else {
        send(&[(VK_CHORD_CTRL, false)]);
        std::thread::sleep(Duration::from_millis(10));
        send(&[(VK_CHORD_WIN, false)]);
    }
}

fn start_menu_selftest(
    n: usize,
    rx: &Receiver<HotkeyEvent>,
    args: &Args,
    home: &GetFg,
    svc: Option<&HotkeyService>,
) -> bool {
    println!("start-menu self-test: {n} chord holds of {TAP_MAX_MS} ms (press/release order varies)");
    let mut opened = 0;
    let mut holds = 0;
    let mut ends = 0;
    let mut other = Vec::new();
    let mut held = Vec::new();
    for i in 0..n {
        if foreground_exe() != args.require_fg {
            println!("ABORT at {i}: foreground changed to {:?}", foreground_exe());
            return false;
        }
        if i == n / 2
            && let Some(s) = svc {
                s.reinstall(); // the hotkey must survive a reinstall mid-run
            }
        chord(i, TAP_MAX_MS);
        if let Some(exe) = watch_for_start(Duration::from_millis(700)) {
            opened += 1;
            println!("  run {i}: {exe} became foreground");
            close_start(home);
            std::thread::sleep(Duration::from_millis(400));
        }
        for ev in drain(rx) {
            match ev {
                HotkeyEvent::HoldStart => holds += 1,
                HotkeyEvent::HoldEnd { held_ms } => {
                    ends += 1;
                    held.push(held_ms);
                }
                e => other.push(e),
            }
        }
    }
    let (mn, mx) = (held.iter().min().copied().unwrap_or(0), held.iter().max().copied().unwrap_or(0));
    println!("START_OPENED {opened}/{n}");
    println!("events: HoldStart {holds}, HoldEnd {ends} (held_ms {mn}..{mx}), other {other:?}");
    let events_ok = svc.is_none() || (holds == n && ends + other.iter().filter(|e| **e == HotkeyEvent::Tap).count() == n);
    opened == 0 && events_ok
}

fn modes_selftest(rx: &Receiver<HotkeyEvent>, args: &Args, home: &GetFg, svc: Option<&HotkeyService>) -> bool {
    let _ = home;
    if svc.is_none() {
        println!("modes self-test needs the service; skipped");
        return true;
    }
    let mut ok = true;
    let mut check = |name: &str, got: Vec<HotkeyEvent>, want: &dyn Fn(&[HotkeyEvent]) -> bool| {
        let pass = want(&got);
        ok &= pass;
        println!("MODE {name}: {} {got:?}", if pass { "OK" } else { "FAIL" });
    };
    let fg_ok = || foreground_exe() == args.require_fg;
    let quiet = Duration::from_millis(DOUBLE_TAP_GAP_MS + 300);

    // Hold of 1 s.
    if !fg_ok() {
        return false;
    }
    chord(0, 1_000);
    check("hold", drain(rx), &|e| matches!(e, [HotkeyEvent::HoldStart, HotkeyEvent::HoldEnd { held_ms }] if *held_ms >= 1_000));
    std::thread::sleep(quiet);

    // Quick tap.
    chord(0, 120);
    check("tap", drain(rx), &|e| e == [HotkeyEvent::HoldStart, HotkeyEvent::Tap]);
    std::thread::sleep(quiet);

    // Double-tap → hands-free; release; next press after the debounce stops it.
    if !fg_ok() {
        return false;
    }
    chord(0, 120);
    std::thread::sleep(Duration::from_millis(200));
    chord(0, 120);
    check("double-tap", drain(rx), &|e| e == [HotkeyEvent::HoldStart, HotkeyEvent::Tap, HotkeyEvent::HandsFreeStart]);
    // Typing during hands-free does not cancel (untagged unassigned key).
    send(&[(VK_OTHER, true), (VK_OTHER, false)]);
    // A press inside the debounce is ignored.
    std::thread::sleep(Duration::from_millis(50));
    chord(0, 60);
    check("debounced press ignored", drain(rx), &|e| e.is_empty());
    std::thread::sleep(Duration::from_millis(HANDSFREE_STOP_DEBOUNCE_MS + 200));
    chord(0, 120);
    check("hands-free stop", drain(rx), &|e| e == [HotkeyEvent::HandsFreeStop]);
    std::thread::sleep(quiet);

    // Another key during a hold cancels.
    if !fg_ok() {
        return false;
    }
    send(&[(VK_CHORD_CTRL, true)]);
    std::thread::sleep(Duration::from_millis(15));
    send(&[(VK_CHORD_WIN, true)]);
    std::thread::sleep(Duration::from_millis(150));
    send(&[(VK_OTHER, true), (VK_OTHER, false)]);
    std::thread::sleep(Duration::from_millis(200));
    send(&[(VK_CHORD_WIN, false), (VK_CHORD_CTRL, false)]);
    check("other key cancels", drain(rx), &|e| matches!(e, [HotkeyEvent::HoldStart, HotkeyEvent::Cancel { .. }]));
    std::thread::sleep(quiet);

    // Tagged keys (Wren's own) do nothing at all.
    send_tagged_keys(&[(VK_CHORD_CTRL, true), (VK_CHORD_WIN, true)]);
    std::thread::sleep(Duration::from_millis(500));
    send_tagged_keys(&[(VK_OTHER, true), (VK_OTHER, false), (VK_CHORD_WIN, false), (VK_CHORD_CTRL, false)]);
    let got = drain(rx);
    check("tagged chord ignored", got, &|e| e.is_empty());
    if watch_for_start(Duration::from_millis(600)).is_some() {
        println!("  (tagged chord opened Start; closing)");
        close_start(home);
    }
    std::thread::sleep(quiet);

    // force_idle mid-hold: the release produces nothing.
    if let Some(s) = svc {
        send(&[(VK_CHORD_CTRL, true)]);
        std::thread::sleep(Duration::from_millis(15));
        send(&[(VK_CHORD_WIN, true)]);
        std::thread::sleep(Duration::from_millis(200));
        s.force_idle();
        std::thread::sleep(Duration::from_millis(400));
        send(&[(VK_CHORD_WIN, false), (VK_CHORD_CTRL, false)]);
        check("force_idle", drain(rx), &|e| e == [HotkeyEvent::HoldStart]);
        println!("last_event_age_ms right after keys: {}", s.last_event_age_ms());
    }
    ok
}

fn passthrough_selftest(rx: &Receiver<HotkeyEvent>, args: &Args) -> bool {
    if foreground_exe() != args.require_fg {
        return false;
    }
    drain(rx);
    // A key Wren swallowed would never show as down in the system's async key state.
    let mut reached_windows = true;
    let mut shortcut = |arrow: u16| {
        send(&[(VK_CHORD_CTRL, true)]);
        std::thread::sleep(Duration::from_millis(15));
        send(&[(VK_CHORD_WIN, true)]);
        std::thread::sleep(Duration::from_millis(40));
        send(&[(arrow, true)]);
        std::thread::sleep(Duration::from_millis(30));
        reached_windows &= [VK_CHORD_CTRL, VK_CHORD_WIN, arrow].iter().all(|vk| async_down(*vk));
        send(&[(arrow, false)]);
        std::thread::sleep(Duration::from_millis(20));
        send(&[(VK_CHORD_WIN, false)]);
        std::thread::sleep(Duration::from_millis(10));
        send(&[(VK_CHORD_CTRL, false)]);
    };
    shortcut(VK_RIGHT);
    std::thread::sleep(Duration::from_millis(900));
    let after_right = foreground_exe();
    shortcut(VK_LEFT); // always sent, so the desktop ends where it started
    std::thread::sleep(Duration::from_millis(900));
    let after_left = foreground_exe();
    let got = drain(rx);
    let cancels = got.iter().filter(|e| matches!(e, HotkeyEvent::Cancel { .. })).count();
    let bad: Vec<_> = got
        .iter()
        .filter(|e| !matches!(e, HotkeyEvent::Cancel { .. } | HotkeyEvent::HoldStart))
        .collect();
    let pass = cancels == 2 && bad.is_empty() && reached_windows;
    println!("PASSTHROUGH {}: events {got:?}", if pass { "OK" } else { "FAIL" });
    println!("  Ctrl, Win and the arrow all showed as down to Windows while held: {reached_windows}");
    println!("  foreground after Ctrl+Win+Right: {after_right:?}; after Ctrl+Win+Left: {after_left:?}");
    pass
}

fn async_down(vk: u16) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32) < 0 }
}

/// Polls the foreground window; returns the Start/Search host's name if it came to the front.
fn watch_for_start(within: Duration) -> Option<String> {
    let t0 = Instant::now();
    while t0.elapsed() < within {
        let exe = foreground_exe();
        if START_HOSTS.contains(&exe.as_str()) {
            return Some(exe);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    None
}

/// Closes Start/Search with Escape. Search ignores keys for a moment after it opens, so wait
/// first and retry until it has really gone.
fn close_start(home: &GetFg) {
    for _ in 0..5 {
        std::thread::sleep(Duration::from_millis(500));
        if !START_HOSTS.contains(&foreground_exe().as_str()) {
            break;
        }
        send_tagged_keys(&[(VK_ESCAPE, true), (VK_ESCAPE, false)]);
    }
    std::thread::sleep(Duration::from_millis(300));
    if foreground_exe() != home.exe {
        unsafe {
            let _ = SetForegroundWindow(HWND(home.hwnd as *mut _));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

struct GetFg {
    hwnd: isize,
    exe: String,
}

impl GetFg {
    fn now() -> Self {
        Self { hwnd: unsafe { GetForegroundWindow() }.0 as isize, exe: foreground_exe() }
    }
}

/// Lower-case file name of the foreground window's process ("" if none or unreadable).
fn foreground_exe() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().unwrap_or("").to_lowercase()
    }
}

/// Names of running processes that look like Willow or Glaido.
fn rival_dictation_apps() -> Vec<String> {
    let mut found = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return found;
        };
        let mut pe = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = Process32FirstW(snap, &mut pe).is_ok();
        while more {
            let len = pe.szExeFile.iter().position(|c| *c == 0).unwrap_or(pe.szExeFile.len());
            let name = String::from_utf16_lossy(&pe.szExeFile[..len]);
            let lower = name.to_lowercase();
            if (lower.contains("willow") || lower.contains("glaido")) && !found.contains(&name) {
                found.push(name);
            }
            more = Process32NextW(snap, &mut pe).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    found
}
