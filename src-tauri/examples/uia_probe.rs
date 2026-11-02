//! Read-only UI Automation probe of the Claude desktop app (Task 9, spec §8): can the correction
//! watcher read its message box quickly enough to be on by default?
//!
//!   cargo run --example uia_probe                 # Claude (claude.exe), and Codex if it is running
//!   cargo run --example uia_probe -- --reads 20
//!
//! What it does, all read-only: finds the app's visible top-level windows, asks UI Automation for
//! the editable elements inside them (no focus change, no input, no value set), and reads each
//! one's text `--reads` times the way the watcher does (fresh facts + TextPattern / ValuePattern),
//! timing every read. It prints lengths and timings only — never any of the text, which may hold
//! the owner's words.
//!
//! Safety: this tool never calls SetFocus, SetValue, SendInput, SetForegroundWindow or anything
//! else that changes the app. The Claude desktop app runs the build session itself.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_I4};
use windows::Win32::UI::Accessibility::{
    IUIAutomationCondition, TreeScope_Descendants, UIA_ControlTypePropertyId, UIA_EditControlTypeId,
    UIA_IsValuePatternAvailablePropertyId, UIA_ValueIsReadOnlyPropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;
use wren_lib::constants::{WATCH_SLOW_CALL_MS, WATCH_UIA_TIMEOUT_MS};
use wren_lib::paste::process_name;
use wren_lib::watcher::uia::{ComMta, Uia, control_type_name};

fn variant_i32(v: i32) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_I4,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { lVal: v },
            }),
        },
    }
}

fn variant_bool(b: bool) -> VARIANT {
    VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: std::mem::ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_BOOL,
                wReserved1: 0,
                wReserved2: 0,
                wReserved3: 0,
                Anonymous: VARIANT_0_0_0 { boolVal: windows::Win32::Foundation::VARIANT_BOOL(if b { -1 } else { 0 }) },
            }),
        },
    }
}

struct Windows(Vec<(HWND, u32)>);

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Windows) };
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        found.0.push((hwnd, pid));
    }
    BOOL(1)
}

/// Visible top-level windows of processes named `exe`, largest first, with their area.
fn windows_of(exe: &str) -> Vec<(HWND, u32, i64)> {
    let mut all = Windows(Vec::new());
    let _ = unsafe { EnumWindows(Some(enum_proc), LPARAM(&mut all as *mut Windows as isize)) };
    let mut out: Vec<(HWND, u32, i64)> = all
        .0
        .into_iter()
        .filter(|(_, pid)| process_name(*pid) == exe)
        .filter(|(h, _)| unsafe { GetWindowTextLengthW(*h) } > 0)
        .map(|(h, pid)| {
            let mut r = RECT::default();
            let _ = unsafe { GetWindowRect(h, &mut r) };
            (h, pid, ((r.right - r.left) as i64) * ((r.bottom - r.top) as i64))
        })
        .collect();
    out.sort_by_key(|w| -w.2);
    out
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn probe_app(uia: &Uia, exe: &str, label: &str, reads: usize) {
    println!("## {label} ({exe})");
    let wins = windows_of(exe);
    if wins.is_empty() {
        println!("- not running (no visible window)\n");
        return;
    }
    println!("- visible windows: {}", wins.len());
    let auto = uia.automation();
    let edit_cond: IUIAutomationCondition = unsafe {
        let edit = auto.CreatePropertyCondition(UIA_ControlTypePropertyId, &variant_i32(UIA_EditControlTypeId.0)).unwrap();
        // Chromium reports TextEdit (and a read-only ValuePattern) on almost every node, so
        // "editable" here means an Edit control or a writable value.
        let has_value = auto.CreatePropertyCondition(UIA_IsValuePatternAvailablePropertyId, &variant_bool(true)).unwrap();
        let writable = auto.CreatePropertyCondition(UIA_ValueIsReadOnlyPropertyId, &variant_bool(false)).unwrap();
        let writable_value = auto.CreateAndCondition(&has_value, &writable).unwrap();
        auto.CreateOrCondition(&edit, &writable_value).unwrap()
    };

    // The focused element right now (only to say whether it is in this app; read-only).
    let focused = uia.focused().ok();

    for (i, (hwnd, pid, area)) in wins.iter().enumerate().take(2) {
        println!("\n### window {} (pid {pid}, {area} px²)", i + 1);
        let root = match unsafe { auto.ElementFromHandle(*hwnd) } {
            Ok(r) => r,
            Err(e) => {
                println!("- ElementFromHandle failed: {e}");
                continue;
            }
        };
        // Chromium builds its accessibility tree the first time a client asks, so the first search
        // may see only the frame. Ask a few times.
        let mut found = Vec::new();
        let mut find_ms = Vec::new();
        for attempt in 0..6 {
            let t = Instant::now();
            let arr = unsafe { root.FindAll(TreeScope_Descendants, &edit_cond) };
            find_ms.push(t.elapsed().as_millis() as u64);
            if let Ok(arr) = arr {
                let n = unsafe { arr.Length() }.unwrap_or(0);
                found.clear();
                for k in 0..n {
                    if let Ok(el) = unsafe { arr.GetElement(k) } {
                        found.push(el);
                    }
                }
            }
            if !found.is_empty() {
                println!("- editable candidates: {} (search {attempt}: {} ms; all searches {find_ms:?} ms)", found.len(), find_ms.last().unwrap());
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if found.is_empty() {
            println!("- no editable element found after {} searches ({find_ms:?} ms)", find_ms.len());
            continue;
        }

        for (k, el) in found.iter().enumerate().take(8) {
            let f = match uia.facts_of(el) {
                Ok(f) => f,
                Err(e) => {
                    println!("- candidate {k}: facts failed: {e}");
                    continue;
                }
            };
            let is_focused = focused.as_ref().is_some_and(|x| !x.runtime_id.is_empty() && x.runtime_id == f.runtime_id);
            println!(
                "- candidate {k}: {} class={:?} framework={:?} value={} (read-only {}) text={} textEdit={} focusable={} focused-now={is_focused}",
                control_type_name(f.facts.control_type),
                f.facts.class_name,
                f.facts.framework,
                f.facts.value_pattern,
                f.facts.value_read_only,
                f.facts.text_pattern,
                f.facts.text_edit_pattern,
                f.facts.keyboard_focusable,
            );
            // The watcher's read: fresh facts (one round trip) + the text.
            let mut times = Vec::new();
            let mut lens = Vec::new();
            let mut fails = 0;
            let mut blank_reads = 0;
            for _ in 0..reads {
                let t = Instant::now();
                let r = uia.facts_of(el).and_then(|f| uia.read_text(&f, None));
                times.push(t.elapsed().as_millis() as u64);
                match r {
                    Ok(Some(text)) => {
                        lens.push(text.chars().count() as i64);
                        // "Blank" = only whitespace or invisible placeholders (how an empty box reads).
                        if wren_lib::watcher::track::is_blank(&text) {
                            blank_reads += 1;
                        }
                    }
                    Ok(None) => lens.push(-1),
                    Err(_) => {
                        fails += 1;
                        lens.push(-2);
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let first_empty = lens.first().is_some_and(|l| *l == 0);
            let mut sorted = times.clone();
            sorted.sort();
            let slow = times.iter().filter(|t| **t > WATCH_SLOW_CALL_MS).count();
            println!(
                "  reads: {reads}, failures {fails}, first read empty: {first_empty}, blank (whitespace only): {blank_reads}, lengths (chars; -1 no text pattern, -2 error): {lens:?}"
            );
            println!(
                "  read ms: p50 {} p95 {} max {} (all {times:?}); slow (> {WATCH_SLOW_CALL_MS} ms): {slow}",
                pct(&sorted, 50.0),
                pct(&sorted, 95.0),
                sorted.last().copied().unwrap_or(0)
            );
        }
    }
    // The focused element itself, timed the way the watcher polls it (read-only).
    let mut times = Vec::new();
    let mut in_app = 0;
    for _ in 0..reads {
        let t = Instant::now();
        if let Ok(f) = uia.focused() {
            let _ = uia.read_text(&f, None);
            if wins.iter().any(|w| w.1 == f.facts.pid) {
                in_app += 1;
            }
        }
        times.push(t.elapsed().as_millis() as u64);
        std::thread::sleep(Duration::from_millis(50));
    }
    times.sort();
    println!(
        "\n- focused-element poll (GetFocusedElement + text): p50 {} p95 {} max {} ms; focus in this app {in_app}/{reads}\n",
        pct(&times, 50.0),
        pct(&times, 95.0),
        times.last().copied().unwrap_or(0)
    );
}

fn main() {
    let mut reads = 20usize;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--reads" => reads = it.next().and_then(|v| v.parse().ok()).expect("--reads N"),
            other => panic!("unknown argument {other}"),
        }
    }
    let _com = ComMta::join();
    let (uia, ms) = Uia::timed(|| Uia::new(WATCH_UIA_TIMEOUT_MS));
    let uia = uia.expect("create a UI Automation client");
    println!("# UI Automation probe (read-only)\n\nclient created in {ms} ms\n");
    probe_app(&uia, "claude.exe", "Claude desktop app", reads);
    probe_app(&uia, "codex.exe", "Codex desktop app", reads);
}
