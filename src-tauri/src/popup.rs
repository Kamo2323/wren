//! The recording popup window (spec §11 "Popup").
//!
//! The popup must NEVER take keyboard focus: if it did, Ctrl+V would land in Wren instead of the
//! app being dictated into. So it is created once at startup (hidden), made non-activating with
//! Win32 extended styles, made click-through, and shown only with `SW_SHOWNOACTIVATE`.
//!
//! **After `create`, never call a Tauri window setter on the popup** (`show`, `hide`,
//! `set_focus`, `set_ignore_cursor_events`, `set_position`…). Tauri/tao keep their own idea of the
//! window's flags; any setter makes tao rewrite both style words from that idea, which hides the
//! popup (tao never saw our Win32 show) and puts `WS_EX_APPWINDOW` back (Alt+Tab, taskbar).
//! Everything here therefore goes straight through Win32.
//!
//! **Threading.** The session thread calls these, so none of them may wait on the main (UI) thread:
//! - the popup's HWND is read once in `create` (main thread) and cached — `WebviewWindow::hwnd()` off
//!   the main thread is a blocking round trip to it;
//! - `show`/`hide` use `SetWindowPos(SWP_ASYNCWINDOWPOS)` and `ShowWindowAsync`, which only post;
//! - changing extended styles (`SetWindowLongPtrW(GWL_EXSTYLE)`) *sends* WM_STYLECHANGING/CHANGED to
//!   the window's thread and waits, so `set_clickable` and the style repair in `show` hand that to the
//!   main thread with `run_on_main_thread` (a post) and return at once;
//! - `emit_state`/`emit_level` post to the webview.
//!
//! `is_own_window` still asks Tauri for each window's handle: call it on the main thread only.
//!
//! **Clicks.** The window is 360 × 120 but the pill is much smaller, and a WebView2 window that is not
//! click-through catches clicks on its transparent pixels too. So the window stays click-through even
//! in the clickable states; while one shows, a small thread watches the cursor (`GetCursorPos`, no
//! messages) and clears `WS_EX_TRANSPARENT` only while the cursor is over the pill, whose rectangle the
//! front end reports with `popup_hit_rect`.

use crate::constants::{POPUP_BOTTOM_MARGIN, POPUP_HIT_SLOP, POPUP_HOVER_POLL_MS, POPUP_WINDOW_H, POPUP_WINDOW_W};
use std::sync::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, SetWindowLongPtrW, SetWindowPos, ShowWindowAsync,
    GWL_EXSTYLE, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TRANSPARENT,
};

pub const POPUP_LABEL: &str = "popup";
/// Event the popup front end listens to for state changes.
pub const POPUP_STATE_EVENT: &str = "popup://state";
/// Event carrying mic level (0..1) about 30 times a second while listening.
pub const POPUP_LEVEL_EVENT: &str = "popup://level";

/// The popup's HWND, read once on the main thread in `create` (0 = not created).
static POPUP_HWND: AtomicIsize = AtomicIsize::new(0);
/// The session wants the popup to take clicks (a clickable state is showing).
static CLICK_WANTED: AtomicBool = AtomicBool::new(false);
/// What the hover thread last asked for (true = not click-through).
static CLICK_APPLIED: AtomicBool = AtomicBool::new(false);
/// The hover thread is running.
static TRACKER_RUNNING: AtomicBool = AtomicBool::new(false);
/// The pill's rectangle inside the popup window, in logical px, as the front end last reported it.
/// None until the first report: the whole window then counts (the old behaviour).
static HIT_RECT: Mutex<Option<HitRect>> = Mutex::new(None);

/// A rectangle inside the popup window in logical (CSS) pixels.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct HitRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

fn hit_rect_lock() -> std::sync::MutexGuard<'static, Option<HitRect>> {
    HIT_RECT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Record where the pill is (the front end reports it whenever the pill changes size).
pub fn set_hit_rect(rect: HitRect) {
    *hit_rect_lock() = Some(rect);
}

/// Is `cursor` over `hit` (logical px inside the window, plus `POPUP_HIT_SLOP`), for a window at
/// `window` = (left, top, right, bottom) in physical px? The scale is physical width ÷ logical width.
pub fn cursor_over(cursor: (i32, i32), window: (i32, i32, i32, i32), hit: Option<HitRect>) -> bool {
    let (left, top, right, bottom) = window;
    let (cx, cy) = cursor;
    if cx < left || cx >= right || cy < top || cy >= bottom {
        return false;
    }
    let Some(r) = hit else { return true };
    let scale = (right - left) as f64 / POPUP_WINDOW_W;
    let x0 = left as f64 + (r.x - POPUP_HIT_SLOP) * scale;
    let y0 = top as f64 + (r.y - POPUP_HIT_SLOP) * scale;
    let x1 = left as f64 + (r.x + r.w + POPUP_HIT_SLOP) * scale;
    let y1 = top as f64 + (r.y + r.h + POPUP_HIT_SLOP) * scale;
    let (fx, fy) = (cx as f64, cy as f64);
    fx >= x0 && fx < x1 && fy >= y0 && fy < y1
}

/// Is the cursor over the pill right now? Reads only (no messages sent): any thread.
fn cursor_over_pill(hwnd: HWND) -> bool {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return false;
        }
        let mut pt = POINT::default();
        let mut rc = RECT::default();
        if GetCursorPos(&mut pt).is_err() || GetWindowRect(hwnd, &mut rc).is_err() {
            return false;
        }
        cursor_over((pt.x, pt.y), (rc.left, rc.top, rc.right, rc.bottom), *hit_rect_lock())
    }
}

/// Create the popup window, hidden and click-through. Call once from `setup` (main thread).
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, POPUP_LABEL, WebviewUrl::App("popup.html".into()))
        .title("Wren")
        .inner_size(POPUP_WINDOW_W, POPUP_WINDOW_H)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build()?;
    // The one Tauri setter we allow, once, at creation: it gives the window WS_EX_LAYERED with the
    // alpha set up for click-through. Our own styles are applied after it, and again on every show.
    let _ = win.set_ignore_cursor_events(true);
    if let Some(hwnd) = hwnd_of(&win) {
        apply_styles(hwnd);
        POPUP_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    }
    Ok(())
}

fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
    // Go through the raw pointer so a windows-crate version difference with Tauri cannot bite.
    win.hwnd().ok().map(|h| HWND(h.0 as *mut core::ffi::c_void))
}

/// The cached popup HWND (no Tauri call, so no main-thread round trip).
fn popup_hwnd(_app: &AppHandle) -> Option<HWND> {
    match POPUP_HWND.load(Ordering::SeqCst) {
        0 => None,
        h => Some(HWND(h as *mut core::ffi::c_void)),
    }
}

/// The extended style the popup should have now (non-activating tool window, never in Alt+Tab or
/// the taskbar), or None if it already has it. Reading styles never blocks.
fn wanted_styles(hwnd: HWND) -> Option<isize> {
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let wanted = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize;
    let unwanted = WS_EX_APPWINDOW.0 as isize;
    let new = (ex | wanted) & !unwanted;
    (new != ex).then_some(new)
}

/// Apply the styles directly. Main thread only (it sends WM_STYLECHANGING to the window's thread).
fn apply_styles(hwnd: HWND) {
    if let Some(new) = wanted_styles(hwnd) {
        unsafe {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
        }
        refresh_frame(hwnd);
    }
}

/// Run `f(hwnd)` on the main thread without waiting for it.
fn on_main(app: &AppHandle, hwnd: HWND, f: impl FnOnce(HWND) + Send + 'static) {
    let raw = hwnd.0 as isize;
    if let Err(err) = app.run_on_main_thread(move || f(HWND(raw as *mut core::ffi::c_void))) {
        tracing::warn!(error = %err, "popup: cannot post to the main thread");
    }
}

fn refresh_frame(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_ASYNCWINDOWPOS,
        );
    }
}

/// Show the popup centred above the bottom of the monitor that holds the foreground window,
/// without activating it. Safe to call repeatedly and from any thread.
pub fn show(app: &AppHandle) {
    let Some(hwnd) = popup_hwnd(app) else { return };
    if wanted_styles(hwnd).is_some() {
        on_main(app, hwnd, apply_styles);
    }
    unsafe {
        let fg = GetForegroundWindow();
        let anchor = if fg.0.is_null() { hwnd } else { fg };
        let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            let scale = dpi_x as f64 / 96.0;
            let w = (POPUP_WINDOW_W * scale).round() as i32;
            let h = (POPUP_WINDOW_H * scale).round() as i32;
            // Centred on the work area, measured up from the monitor's bottom edge (not the work
            // area's), so the pill sits on the top edge of a bottom taskbar, as Glaido's does. A
            // taskbar at the top or side leaves rcWork.bottom == rcMonitor.bottom, so this holds
            // there too. HWND_TOPMOST is re-asserted on every show so the pill draws above the taskbar.
            let work = info.rcWork;
            let x = work.left + ((work.right - work.left) - w) / 2;
            let y = info.rcMonitor.bottom - h - (POPUP_BOTTOM_MARGIN * scale).round() as i32;
            let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS;
            // Moving onto a monitor with a different DPI makes the window resize itself to Windows'
            // suggested rect; a second identical call (no DPI change any more) puts it back.
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
        }
        let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
    }
}

/// Hide the popup. Any thread.
pub fn hide(app: &AppHandle) {
    if let Some(hwnd) = popup_hwnd(app) {
        unsafe {
            let _ = ShowWindowAsync(hwnd, SW_HIDE);
        }
    }
}

/// Let the popup receive clicks (for "click to copy", "Undo", "×") or pass them all through.
/// `true` does not make the whole window clickable: it starts the hover thread, which lets the window
/// take clicks only while the cursor is over the pill. Any thread; returns at once.
pub fn set_clickable(app: &AppHandle, clickable: bool) {
    let Some(hwnd) = popup_hwnd(app) else { return };
    CLICK_WANTED.store(clickable, Ordering::SeqCst);
    if !clickable {
        CLICK_APPLIED.store(false, Ordering::SeqCst);
        post_click_style(app, hwnd, false);
        return;
    }
    if TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
        return; // already watching
    }
    let thread_app = app.clone();
    let raw = hwnd.0 as isize;
    let spawned = std::thread::Builder::new().name("wren-popup-hover".into()).spawn(move || {
        let hwnd = HWND(raw as *mut core::ffi::c_void);
        let poll = std::time::Duration::from_millis(POPUP_HOVER_POLL_MS);
        loop {
            if !CLICK_WANTED.load(Ordering::SeqCst) {
                TRACKER_RUNNING.store(false, Ordering::SeqCst);
                // A set_clickable(true) that came in just after the check above found this thread
                // still running and returned: carry on watching for it.
                if CLICK_WANTED.load(Ordering::SeqCst) && !TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
                    continue;
                }
                return;
            }
            let over = cursor_over_pill(hwnd);
            if over != CLICK_APPLIED.load(Ordering::SeqCst) {
                CLICK_APPLIED.store(over, Ordering::SeqCst);
                post_click_style(&thread_app, hwnd, over);
            }
            std::thread::sleep(poll);
        }
    });
    if let Err(err) = spawned {
        TRACKER_RUNNING.store(false, Ordering::SeqCst);
        tracing::warn!(error = %err, "popup: cannot start the hover thread; the whole window takes clicks");
        post_click_style(app, hwnd, true);
    }
}

/// Toggle `WS_EX_TRANSPARENT` on the main thread (posted); `WS_EX_LAYERED` from creation stays. A
/// request to take clicks that lands after the session said "no more clicks" is dropped there.
fn post_click_style(app: &AppHandle, hwnd: HWND, take_clicks: bool) {
    on_main(app, hwnd, move |hwnd| {
        let take_clicks = take_clicks && CLICK_WANTED.load(Ordering::SeqCst);
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let transparent = WS_EX_TRANSPARENT.0 as isize;
            let new = if take_clicks { ex & !transparent } else { ex | transparent };
            if new != ex {
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new);
                refresh_frame(hwnd);
            }
        }
        apply_styles(hwnd);
    });
}

/// True if `hwnd` is one of Wren's own windows (used to refuse pasting into ourselves). Asks Tauri
/// for each window's handle: main thread only (the session uses a list cached at start-up).
pub fn is_own_window(app: &AppHandle, hwnd: isize) -> bool {
    app.webview_windows()
        .values()
        .filter_map(hwnd_of)
        .any(|h| h.0 as isize == hwnd)
}

/// Send a state payload to the popup front end.
pub fn emit_state<S: Serialize + Clone>(app: &AppHandle, state: &S) {
    let _ = app.emit_to(POPUP_LABEL, POPUP_STATE_EVENT, state.clone());
}

/// Send a mic level (0..1) to the popup front end.
pub fn emit_level(app: &AppHandle, level: f32) {
    let _ = app.emit_to(POPUP_LABEL, POPUP_LEVEL_EVENT, level);
}

/// Live check of the rules above, run from the calling thread (use a worker thread). Shows, toggles
/// and hides the popup and checks visibility, styles and that the foreground window never changed.
/// Returns the list of failures (empty = pass). Used by `--popup-check` and the self-test.
pub fn self_check(app: &AppHandle) -> Vec<String> {
    use std::time::Duration;
    let mut failures = Vec::new();
    let Some(hwnd) = popup_hwnd(app) else {
        return vec!["popup window not found".into()];
    };
    let settle = || std::thread::sleep(Duration::from_millis(300));
    let ex = || unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let visible = || unsafe { IsWindowVisible(hwnd).as_bool() };
    let check_styles = |stage: &str, failures: &mut Vec<String>| {
        let e = ex();
        if e & WS_EX_NOACTIVATE.0 as isize == 0 {
            failures.push(format!("{stage}: WS_EX_NOACTIVATE missing (0x{e:X})"));
        }
        if e & WS_EX_TOOLWINDOW.0 as isize == 0 {
            failures.push(format!("{stage}: WS_EX_TOOLWINDOW missing (0x{e:X})"));
        }
        if e & WS_EX_APPWINDOW.0 as isize != 0 {
            failures.push(format!("{stage}: WS_EX_APPWINDOW present (0x{e:X})"));
        }
    };
    let fg_before = unsafe { GetForegroundWindow() };

    show(app);
    settle();
    if !visible() {
        failures.push("after show: not visible".into());
    }
    check_styles("after show", &mut failures);
    if ex() & WS_EX_TRANSPARENT.0 as isize == 0 {
        failures.push("after show: not click-through".into());
    }

    // Clickable: the window takes clicks only while the cursor is over the pill. Check a pill that
    // fills the window and a tiny one in a corner; the expected answer follows the real cursor.
    let saved_rect = *hit_rect_lock();
    let takes_clicks = |ex: isize| ex & WS_EX_TRANSPARENT.0 as isize == 0;
    set_hit_rect(HitRect { x: 0.0, y: 0.0, w: POPUP_WINDOW_W, h: POPUP_WINDOW_H });
    set_clickable(app, true);
    settle();
    if !visible() {
        failures.push("after set_clickable(true): hidden".into());
    }
    let over = cursor_over_pill(hwnd);
    if takes_clicks(ex()) != over {
        failures.push(format!("set_clickable(true), pill fills the window: takes clicks {}, cursor over it {over}", takes_clicks(ex())));
    }
    check_styles("after set_clickable(true)", &mut failures);
    set_hit_rect(HitRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 });
    settle();
    let over = cursor_over_pill(hwnd);
    if takes_clicks(ex()) != over {
        failures.push(format!("set_clickable(true), tiny pill: takes clicks {}, cursor over it {over}", takes_clicks(ex())));
    }
    *hit_rect_lock() = saved_rect;

    set_clickable(app, false);
    settle();
    if ex() & WS_EX_TRANSPARENT.0 as isize == 0 {
        failures.push("after set_clickable(false): not click-through".into());
    }

    hide(app);
    settle();
    if visible() {
        failures.push("after hide: still visible".into());
    }

    show(app);
    settle();
    if !visible() {
        failures.push("after second show: not visible".into());
    }
    hide(app);
    settle();
    if visible() {
        failures.push("after second hide: still visible".into());
    }

    let fg_after = unsafe { GetForegroundWindow() };
    if fg_after != fg_before {
        failures.push("foreground window changed during the check".into());
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: (i32, i32, i32, i32) = (1000, 900, 1360, 1020); // 360 × 120 at 100 %
    const PILL: HitRect = HitRect { x: 122.0, y: 68.0, w: 116.0, h: 38.0 };

    #[test]
    fn only_the_pill_and_its_slop_take_clicks() {
        assert!(cursor_over((1000 + 180, 900 + 87), WIN, Some(PILL)), "centre of the pill");
        assert!(cursor_over((1000 + 119, 900 + 87), WIN, Some(PILL)), "inside the 4 px slop");
        assert!(!cursor_over((1000 + 180, 900 + 40), WIN, Some(PILL)), "above the pill");
        assert!(!cursor_over((1000 + 20, 900 + 87), WIN, Some(PILL)), "beside the pill");
        assert!(!cursor_over((990, 950), WIN, Some(PILL)), "outside the window");
    }

    #[test]
    fn before_the_first_report_the_whole_window_counts() {
        assert!(cursor_over((1000 + 5, 900 + 5), WIN, None));
        assert!(!cursor_over((999, 905), WIN, None));
    }

    #[test]
    fn follows_the_display_scale() {
        let win150 = (0, 0, 540, 180); // 150 %
        assert!(cursor_over((270, 130), win150, Some(PILL)));
        assert!(!cursor_over((270, 60), win150, Some(PILL)), "above the pill at 150 %");
    }
}
