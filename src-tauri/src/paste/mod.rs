//! Paste (spec §7): put the final text into the app that was in front when the dictation stopped,
//! through the clipboard, with proof that the app read it — never by typing keys (a typed newline
//! would send a chat message).
//!
//! `paste_text` checks, in order: Wren's own window → the chord keys are up (≤ `KEYUP_WAIT_MAX_MS`)
//! → the same window is still in front. Then it backs up the clipboard, publishes the text with
//! delayed rendering, sends Ctrl+V and waits for the read receipt; if none comes, Ctrl+Shift+V once.
//! The keys and the focus are checked again immediately before *each* chord: publishing can wait up
//! to `CLIPBOARD_RESTORE_MS` for the previous paste's restore, and he may have started the next
//! dictation meanwhile (Ctrl+V with Ctrl+Win held is Win+Ctrl+V, and the injected Ctrl-up would end
//! the new hold).
//! After a read the backup is restored `CLIPBOARD_RESTORE_MS` later (unless someone else copied
//! meanwhile). With no read the text is left on the clipboard.
//!
//! What the caller does with each result (Task 8):
//! - `Pasted` — done.
//! - `FocusChanged`, `OwnWindow`, `KeysStillHeld` — nothing was sent to any app; the caller shows
//!   "Not pasted — click to copy" and calls `copy_text` (spec §7). For `FocusChanged` or
//!   `KeysStillHeld` detected *after* Ctrl+V (before the Ctrl+Shift+V retry) the text is also
//!   already on the clipboard.
//! - `Failed(REASON_NOT_READ)` — Ctrl+V and Ctrl+Shift+V were sent and nothing read the clipboard;
//!   the text is on the clipboard ("Copied — press Ctrl+V").
//! - `Failed(other)` — the clipboard could not be used (busy, empty text…); nothing was sent.

#[cfg(windows)]
pub mod clipboard;
#[cfg(target_os = "macos")]
pub mod clipboard_macos;

use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::constants::{KEYUP_POLL_MS, KEYUP_WAIT_MAX_MS};
use crate::hotkey::chord_keys_physically_up;
#[cfg(windows)]
pub use clipboard::{copy_text, flush};
#[cfg(target_os = "macos")]
pub use clipboard_macos::{copy_text, flush};

/// The reason in `PasteResult::Failed` when no app read the clipboard (the text is left on it).
pub const REASON_NOT_READ: &str = "the app did not read the clipboard; the text is left on it";

/// The window that was in front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundInfo {
    pub hwnd: isize,
    pub pid: u32,
    /// Process / application name (e.g. `"claude.exe"` on Windows, `"Claude"` on macOS); empty if unknown.
    pub process_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteResult {
    Pasted,
    FocusChanged,
    KeysStillHeld,
    OwnWindow,
    /// Not sent: the pre-paste check (ruling 24) found nothing editable in focus (Task 9).
    NotEditable,
    Failed(String),
}

/// Which paste shortcut produced the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    CtrlV,
    CtrlShiftV,
    CmdV,
}

/// Diagnostics of the last read receipt (no text): which chord, and ms from sending it to the read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receipt {
    pub chord: Chord,
    pub latency_ms: u64,
}

static LAST_RECEIPT: Mutex<Option<Receipt>> = Mutex::new(None);

/// The last successful paste's receipt (for the probe and the self-test).
pub fn last_receipt() -> Option<Receipt> {
    *LAST_RECEIPT.lock()
}

/// Polls the physical chord keys until both are up, for at most `KEYUP_WAIT_MAX_MS`.
fn wait_for_keys_up() -> bool {
    let start = Instant::now();
    loop {
        if chord_keys_physically_up() {
            return true;
        }
        if start.elapsed() >= Duration::from_millis(KEYUP_WAIT_MAX_MS) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(KEYUP_POLL_MS));
    }
}

/// How the clipboard part of a paste went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Read(Receipt),
    NotRead,
    FocusLost,
    /// The chord keys were down right before a chord (and stayed down for `KEYUP_WAIT_MAX_MS`);
    /// nothing more was sent and the text is left on the clipboard.
    KeysHeld,
    Error(String),
}

// ============================================================================================
// Windows implementation
// ============================================================================================
#[cfg(windows)]
mod platform {
    use super::*;
    use crate::constants::{PASTE_READ_POLL_MS, PASTE_READ_WAIT_MS};
    use crate::hotkey::send_tagged_keys;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_SHIFT, VK_V};
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    use windows::core::PWSTR;

    pub fn foreground() -> ForegroundInfo {
        let hwnd = unsafe { GetForegroundWindow() };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        ForegroundInfo { hwnd: hwnd.0 as isize, pid, process_name: process_name(pid) }
    }

    pub fn process_name(pid: u32) -> String {
        if pid == 0 {
            return String::new();
        }
        let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = unsafe { QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
        let _ = unsafe { CloseHandle(handle) };
        if ok.is_err() {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_lowercase()
    }

    pub fn paste_text(text: &str, target: &ForegroundInfo, is_own: &dyn Fn(isize) -> bool) -> PasteResult {
        if text.trim().is_empty() {
            return PasteResult::Failed("nothing to paste".into());
        }
        if is_own(target.hwnd) || target.pid == std::process::id() {
            return PasteResult::OwnWindow;
        }
        if !wait_for_keys_up() {
            tracing::info!("chord keys still held; not pasting");
            return PasteResult::KeysStillHeld;
        }
        let now = foreground();
        if is_own(now.hwnd) || (now.pid == std::process::id() && now.pid != 0) {
            return PasteResult::OwnWindow;
        }
        if now.hwnd != target.hwnd {
            tracing::info!(target = %target.process_name, now = %now.process_name, "foreground changed; not pasting");
            return PasteResult::FocusChanged;
        }
        let still_target = || unsafe { GetForegroundWindow() }.0 as isize == target.hwnd;
        let outcome = paste_via_clipboard(text, &send_chord, &still_target, &wait_for_keys_up);
        let chars = text.chars().count();
        match outcome {
            Outcome::Read(receipt) => {
                tracing::info!(chars, chord = ?receipt.chord, latency_ms = receipt.latency_ms, app = %target.process_name, "pasted");
                *LAST_RECEIPT.lock() = Some(receipt);
                PasteResult::Pasted
            }
            Outcome::NotRead => {
                tracing::warn!(chars, app = %target.process_name, "no clipboard read after Ctrl+V and Ctrl+Shift+V; text left on the clipboard");
                PasteResult::Failed(REASON_NOT_READ.into())
            }
            Outcome::KeysHeld => {
                tracing::info!(chars, "chord keys held again before a paste chord; text left on the clipboard");
                PasteResult::KeysStillHeld
            }
            Outcome::FocusLost => {
                tracing::info!(chars, "foreground changed before the retry; text left on the clipboard");
                PasteResult::FocusChanged
            }
            Outcome::Error(e) => {
                tracing::warn!(error = %e, "paste failed");
                PasteResult::Failed(e)
            }
        }
    }

    pub fn chord_keys(chord: Chord) -> Vec<(u16, bool)> {
        match chord {
            Chord::CtrlV | Chord::CmdV => vec![(VK_CONTROL.0, true), (VK_V.0, true), (VK_V.0, false), (VK_CONTROL.0, false)],
            Chord::CtrlShiftV => vec![
                (VK_CONTROL.0, true),
                (VK_SHIFT.0, true),
                (VK_V.0, true),
                (VK_V.0, false),
                (VK_SHIFT.0, false),
                (VK_CONTROL.0, false),
            ],
        }
    }

    fn send_chord(chord: Chord) {
        send_tagged_keys(&chord_keys(chord));
    }

    pub fn paste_via_clipboard(
        text: &str,
        send: &dyn Fn(Chord),
        still_focused: &dyn Fn() -> bool,
        keys_up: &dyn Fn() -> bool,
    ) -> Outcome {
        let tx = match clipboard::Transaction::start(text) {
            Ok(tx) => tx,
            Err(e) => return Outcome::Error(e.to_string()),
        };
        let max = Duration::from_millis(PASTE_READ_WAIT_MS);
        let poll = Duration::from_millis(PASTE_READ_POLL_MS);
        for chord in [Chord::CtrlV, Chord::CtrlShiftV] {
            if !keys_up() {
                tx.leave_text();
                return Outcome::KeysHeld;
            }
            if !still_focused() {
                tx.leave_text();
                return Outcome::FocusLost;
            }
            let sent_at = Instant::now();
            send(chord);
            if tx.wait_read(max, poll) {
                let latency_ms = tx.read_at().map(|t| t.saturating_duration_since(sent_at).as_millis() as u64).unwrap_or(0);
                tx.restore_later();
                return Outcome::Read(Receipt { chord, latency_ms });
            }
        }
        tx.leave_text();
        Outcome::NotRead
    }
}

// ============================================================================================
// macOS implementation
// ============================================================================================
#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::constants::INJECT_TAG;
    use objc2_app_kit::NSWorkspace;
    use std::ffi::c_void;

    type CGEventSourceRef = *mut c_void;
    type CGEventRef = *mut c_void;
    type CGEventFlags = u64;

    const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM: i32 = 1;
    const K_CG_SESSION_EVENT_TAP: u32 = 1;
    const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 1 << 20;
    const K_CG_EVENT_SOURCE_USER_DATA: u32 = 42;
    const K_VK_ANSI_V: u16 = 9;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceCreate(state_id: i32) -> CGEventSourceRef;
        fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, virtual_key: u16, key_down: bool) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: CGEventFlags);
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
        fn CGEventPost(tap: u32, event: CGEventRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    /// Frontmost application PID, friendly name, and a token encoding PID (+ AX window hash if
    /// accessible) so switching apps OR switching windows inside an app is caught as `FocusChanged`.
    pub fn foreground() -> ForegroundInfo {
        let ws = NSWorkspace::sharedWorkspace();
        let Some(app) = ws.frontmostApplication() else {
            return ForegroundInfo { hwnd: 0, pid: 0, process_name: String::new() };
        };
        let pid = app.processIdentifier().max(0) as u32;
        let name = app
            .localizedName()
            .map(|n| n.to_string())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| process_name(pid));
        let win_hash = crate::watcher::ax_macos::focused_window_token(pid).unwrap_or(0);
        let hwnd = ((pid as isize) << 32) | (win_hash as isize & 0xFFFF_FFFF);
        ForegroundInfo { hwnd, pid, process_name: name }
    }

    pub fn process_name(pid: u32) -> String {
        crate::engine::process_image(pid)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }

    fn send_cmd_v() -> Result<(), String> {
        unsafe {
            let source = CGEventSourceCreate(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM);
            let down = CGEventCreateKeyboardEvent(source, K_VK_ANSI_V, true);
            let up = CGEventCreateKeyboardEvent(source, K_VK_ANSI_V, false);
            if down.is_null() || up.is_null() {
                if !down.is_null() {
                    CFRelease(down as *const c_void);
                }
                if !up.is_null() {
                    CFRelease(up as *const c_void);
                }
                if !source.is_null() {
                    CFRelease(source as *const c_void);
                }
                return Err("CGEventCreateKeyboardEvent returned null".into());
            }
            CGEventSetFlags(down, K_CG_EVENT_FLAG_MASK_COMMAND);
            CGEventSetIntegerValueField(down, K_CG_EVENT_SOURCE_USER_DATA, INJECT_TAG as i64);
            CGEventSetFlags(up, K_CG_EVENT_FLAG_MASK_COMMAND);
            CGEventSetIntegerValueField(up, K_CG_EVENT_SOURCE_USER_DATA, INJECT_TAG as i64);

            CGEventPost(K_CG_SESSION_EVENT_TAP, down);
            std::thread::sleep(Duration::from_millis(12));
            CGEventPost(K_CG_SESSION_EVENT_TAP, up);

            CFRelease(down as *const c_void);
            CFRelease(up as *const c_void);
            if !source.is_null() {
                CFRelease(source as *const c_void);
            }
        }
        Ok(())
    }

    pub fn paste_text(text: &str, target: &ForegroundInfo, is_own: &dyn Fn(isize) -> bool) -> PasteResult {
        if text.trim().is_empty() {
            return PasteResult::Failed("nothing to paste".into());
        }
        if is_own(target.hwnd) || target.pid == std::process::id() {
            return PasteResult::OwnWindow;
        }
        if !wait_for_keys_up() {
            tracing::info!("chord keys still held; not pasting");
            return PasteResult::KeysStillHeld;
        }
        let now = foreground();
        if is_own(now.hwnd) || (now.pid == std::process::id() && now.pid != 0) {
            return PasteResult::OwnWindow;
        }
        if now.pid != target.pid || (target.hwnd != 0 && now.hwnd != target.hwnd) {
            tracing::info!(target = %target.process_name, now = %now.process_name, "foreground changed; not pasting");
            return PasteResult::FocusChanged;
        }
        let tx = match clipboard_macos::Transaction::start(text) {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!(error = %e, "paste failed");
                return PasteResult::Failed(e.to_string());
            }
        };
        if !wait_for_keys_up() {
            tx.leave_text();
            return PasteResult::KeysStillHeld;
        }
        let check = foreground();
        if check.pid != target.pid || (target.hwnd != 0 && check.hwnd != target.hwnd) {
            tx.leave_text();
            return PasteResult::FocusChanged;
        }
        let sent_at = Instant::now();
        if let Err(e) = send_cmd_v() {
            tx.leave_text();
            tracing::warn!(error = %e, "⌘V injection failed; text left on the clipboard");
            return PasteResult::Failed(e);
        }
        let latency_ms = sent_at.elapsed().as_millis() as u64;
        tx.restore_later();
        let receipt = Receipt { chord: Chord::CmdV, latency_ms };
        *LAST_RECEIPT.lock() = Some(receipt);
        tracing::info!(chars = text.chars().count(), chord = ?Chord::CmdV, latency_ms, app = %target.process_name, "pasted");
        PasteResult::Pasted
    }
}

pub use platform::{foreground, paste_text, process_name};
#[cfg(windows)]
pub use platform::{chord_keys, paste_via_clipboard};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn chords_are_balanced_and_ctrl_wraps_v() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_SHIFT};
        for chord in [Chord::CtrlV, Chord::CtrlShiftV] {
            let keys = chord_keys(chord);
            assert_eq!(keys.first(), Some(&(VK_CONTROL.0, true)));
            assert_eq!(keys.last(), Some(&(VK_CONTROL.0, false)));
            for &(vk, _) in &keys {
                let downs = keys.iter().filter(|k| k.0 == vk && k.1).count();
                let ups = keys.iter().filter(|k| k.0 == vk && !k.1).count();
                assert_eq!((downs, ups), (1, 1), "{vk:#x}");
            }
        }
        assert_eq!(chord_keys(Chord::CtrlV).len(), 4);
        assert!(chord_keys(Chord::CtrlShiftV).contains(&(VK_SHIFT.0, true)));
    }

    #[test]
    fn empty_text_is_never_pasted() {
        let target = ForegroundInfo { hwnd: 1, pid: 1, process_name: "x.exe".into() };
        for text in ["", "  ", "\n"] {
            assert_eq!(paste_text(text, &target, &|_| false), PasteResult::Failed("nothing to paste".into()));
        }
    }

    #[test]
    fn own_window_is_refused_before_anything_else() {
        let target = ForegroundInfo { hwnd: 42, pid: 1, process_name: "wren.exe".into() };
        assert_eq!(paste_text("hello", &target, &|h| h == 42), PasteResult::OwnWindow);
        let own_pid = ForegroundInfo { hwnd: 43, pid: std::process::id(), process_name: String::new() };
        assert_eq!(paste_text("hello", &own_pid, &|_| false), PasteResult::OwnWindow);
    }

    #[test]
    fn a_window_that_is_not_in_front_gets_focus_changed() {
        if !chord_keys_physically_up() {
            return;
        }
        let target = ForegroundInfo { hwnd: 7, pid: 1, process_name: "x.exe".into() };
        assert_eq!(paste_text("hello", &target, &|_| false), PasteResult::FocusChanged);
    }

    #[test]
    fn foreground_reports_a_process() {
        let fg = foreground();
        if fg.hwnd != 0 {
            assert!(fg.pid != 0);
        }
    }
}
