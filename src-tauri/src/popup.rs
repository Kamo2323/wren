//! The recording popup window (spec §11 "Popup").
//!
//! The popup must NEVER take keyboard focus: if it did, Ctrl+V / ⌘V would land in Wren instead of
//! the app being dictated into. It is created once at startup (hidden), made non-activating and
//! click-through, and shown without activating the app.
//!
//! - Windows: direct Win32 (`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT`,
//!   `SetWindowPos(SWP_NOACTIVATE)`, `ShowWindowAsync(SW_SHOWNOACTIVATE)`).
//! - macOS: native `NSWindow` (`NSStatusWindowLevel`, `CanJoinAllSpaces | FullScreenAuxiliary |
//!   Stationary | IgnoresCycle`, `orderFrontRegardless`, `setIgnoresMouseEvents`).
//!
//! Clicks: the window is 360 × 120 so the pill can spring inside it, while remaining click-through
//! everywhere except over the pill itself during interactive states.

use crate::constants::{POPUP_BOTTOM_MARGIN, POPUP_HIT_SLOP, POPUP_HOVER_POLL_MS, POPUP_WINDOW_H, POPUP_WINDOW_W};
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
#[cfg(windows)]
use tauri::Manager;
use tauri::{AppHandle, Emitter, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const POPUP_LABEL: &str = "popup";
/// Event the popup front end listens to for state changes.
pub const POPUP_STATE_EVENT: &str = "popup://state";
/// Event carrying mic level (0..1) about 30 times a second while listening.
pub const POPUP_LEVEL_EVENT: &str = "popup://level";

/// The session wants the popup to take clicks (a clickable state is showing).
static CLICK_WANTED: AtomicBool = AtomicBool::new(false);
/// What the hover thread last asked for (true = not click-through).
static CLICK_APPLIED: AtomicBool = AtomicBool::new(false);
/// The hover thread is running.
static TRACKER_RUNNING: AtomicBool = AtomicBool::new(false);
/// The pill's rectangle inside the popup window, in logical px, as the front end last reported it.
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
/// `window` = (left, top, right, bottom) in physical px (or logical pt at scale 1.0)?
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

/// Send a state payload to the popup front end.
pub fn emit_state<S: Serialize + Clone>(app: &AppHandle, state: &S) {
    let _ = app.emit_to(POPUP_LABEL, POPUP_STATE_EVENT, state.clone());
}

/// Send a mic level (0..1) to the popup front end.
pub fn emit_level(app: &AppHandle, level: f32) {
    let _ = app.emit_to(POPUP_LABEL, POPUP_LEVEL_EVENT, level);
}

// ============================================================================================
// Windows implementation
// ============================================================================================
#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, SetWindowLongPtrW,
        SetWindowPos, ShowWindowAsync, GWL_EXSTYLE, HWND_TOPMOST, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNOACTIVATE,
        WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    static POPUP_HWND: AtomicIsize = AtomicIsize::new(0);

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
        let _ = win.set_ignore_cursor_events(true);
        if let Some(hwnd) = hwnd_of(&win) {
            apply_styles(hwnd);
            POPUP_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        }
        Ok(())
    }

    fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
        win.hwnd().ok().map(|h| HWND(h.0 as *mut core::ffi::c_void))
    }

    fn popup_hwnd(_app: &AppHandle) -> Option<HWND> {
        match POPUP_HWND.load(Ordering::SeqCst) {
            0 => None,
            h => Some(HWND(h as *mut core::ffi::c_void)),
        }
    }

    fn wanted_styles(hwnd: HWND) -> Option<isize> {
        let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
        let wanted = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) as isize;
        let unwanted = WS_EX_APPWINDOW.0 as isize;
        let new = (ex | wanted) & !unwanted;
        (new != ex).then_some(new)
    }

    fn apply_styles(hwnd: HWND) {
        if let Some(new) = wanted_styles(hwnd) {
            unsafe { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new) };
            refresh_frame(hwnd);
        }
    }

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
                let work = info.rcWork;
                let x = work.left + ((work.right - work.left) - w) / 2;
                let y = info.rcMonitor.bottom - h - (POPUP_BOTTOM_MARGIN * scale).round() as i32;
                let flags = SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_ASYNCWINDOWPOS;
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, flags);
            }
            let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
        }
    }

    pub fn hide(app: &AppHandle) {
        if let Some(hwnd) = popup_hwnd(app) {
            unsafe {
                let _ = ShowWindowAsync(hwnd, SW_HIDE);
            }
        }
    }

    pub fn set_clickable(app: &AppHandle, clickable: bool) {
        let Some(hwnd) = popup_hwnd(app) else { return };
        CLICK_WANTED.store(clickable, Ordering::SeqCst);
        if !clickable {
            CLICK_APPLIED.store(false, Ordering::SeqCst);
            post_click_style(app, hwnd, false);
            return;
        }
        if TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        let thread_app = app.clone();
        let raw = hwnd.0 as isize;
        let spawned = std::thread::Builder::new().name("wren-popup-hover".into()).spawn(move || {
            let hwnd = HWND(raw as *mut core::ffi::c_void);
            let poll = std::time::Duration::from_millis(POPUP_HOVER_POLL_MS);
            loop {
                if !CLICK_WANTED.load(Ordering::SeqCst) {
                    TRACKER_RUNNING.store(false, Ordering::SeqCst);
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

    pub fn is_own_window(app: &AppHandle, hwnd: isize) -> bool {
        app.webview_windows()
            .values()
            .filter_map(hwnd_of)
            .any(|h| h.0 as isize == hwnd)
    }

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
            failures.push(format!(
                "set_clickable(true), pill fills the window: takes clicks {}, cursor over it {over}",
                takes_clicks(ex())
            ));
        }
        check_styles("after set_clickable(true)", &mut failures);
        set_hit_rect(HitRect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 });
        settle();
        let over = cursor_over_pill(hwnd);
        if takes_clicks(ex()) != over {
            failures.push(format!(
                "set_clickable(true), tiny pill: takes clicks {}, cursor over it {over}",
                takes_clicks(ex())
            ));
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
}

// ============================================================================================
// macOS implementation (AppKit NSWindow)
// ============================================================================================
#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use objc2::{ClassType, MainThreadOnly, define_class};
    use objc2_app_kit::{
        NSColor, NSEvent, NSPanel, NSResponder, NSScreen, NSStatusWindowLevel, NSWindow,
        NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    };
    use objc2_foundation::{MainThreadMarker, NSObject, NSPoint, NSRect, NSSize};

    unsafe extern "C" {
        fn object_setClass(obj: *mut core::ffi::c_void, cls: *const core::ffi::c_void) -> *const core::ffi::c_void;
        fn object_getClass(obj: *const core::ffi::c_void) -> *const core::ffi::c_void;
        fn class_getInstanceSize(cls: *const core::ffi::c_void) -> usize;
    }

    // Real AppKit NSPanel subclass so macOS honors `NSWindowStyleMask::NonactivatingPanel`,
    // `setFloatingPanel(true)`, and `setWorksWhenModal(true)` without ever activating Wren or
    // stealing keyboard focus from the target application. Zero extra ivars so swapping a
    // `TaoWindow` (`NSWindow` subclass, same 520-byte base layout as `NSPanel`) is layout-safe.
    define_class!(
        #[unsafe(super(NSPanel, NSWindow, NSResponder, NSObject))]
        #[thread_kind = MainThreadOnly]
        #[name = "WrenPopupPanel"]
        struct WrenPopupPanel;

        impl WrenPopupPanel {
            #[unsafe(method(canBecomeKeyWindow))]
            fn can_become_key_window(&self) -> bool {
                false
            }

            #[unsafe(method(canBecomeMainWindow))]
            fn can_become_main_window(&self) -> bool {
                false
            }
        }
    );

    static POPUP_NSWIN: AtomicIsize = AtomicIsize::new(0);
    /// Cached popup frame in Cocoa screen points `(left, bottom, right, top)` for lock-free hover checks.
    static POPUP_FRAME: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
    static POPUP_VISIBLE: AtomicBool = AtomicBool::new(false);

    fn ns_window_ptr(win: &WebviewWindow) -> Option<isize> {
        win.ns_window().ok().map(|p| p as isize)
    }

    fn configure_ns_window(raw: isize) {
        if raw == 0 {
            return;
        }
        unsafe {
            // Verify runtime instance-size compatibility before swizzling `TaoWindow` (`NSWindow`
            // subclass with a single `AtomicBool` ivar that has no `Drop` glue) into
            // `WrenPopupPanel : NSPanel` (zero extra ivars). On Apple Silicon macOS 13–26,
            // `sizeof(NSPanel) == sizeof(NSWindow)` and `WrenPopupPanel <= TaoWindow`.
            let orig_cls = object_getClass(raw as *const core::ffi::c_void);
            let panel_cls = WrenPopupPanel::class() as *const _ as *const core::ffi::c_void;
            let ns_panel_size = class_getInstanceSize(NSPanel::class() as *const _ as *const core::ffi::c_void);
            let ns_win_size = class_getInstanceSize(NSWindow::class() as *const _ as *const core::ffi::c_void);
            let target_size = class_getInstanceSize(panel_cls);
            let orig_size = class_getInstanceSize(orig_cls);

            if ns_panel_size == ns_win_size && target_size <= orig_size {
                object_setClass(raw as *mut core::ffi::c_void, panel_cls);
                let panel: &NSPanel = &*(raw as *const NSPanel);
                panel.setStyleMask(NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel);
                panel.setFloatingPanel(true);
                panel.setBecomesKeyOnlyIfNeeded(true);
                panel.setWorksWhenModal(true);
            } else {
                tracing::warn!(
                    ns_panel_size,
                    ns_win_size,
                    target_size,
                    orig_size,
                    "skipping NSPanel swizzle due to unexpected AppKit instance size"
                );
            }

            let win: &NSWindow = &*(raw as *const NSWindow);
            win.setLevel(NSStatusWindowLevel);
            win.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            win.setOpaque(false);
            win.setBackgroundColor(Some(&NSColor::clearColor()));
            win.setHasShadow(false);
            win.setHidesOnDeactivate(false);
            win.setCanHide(false);
            win.setIgnoresMouseEvents(true);
        }
    }

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
        let _ = win.set_ignore_cursor_events(true);
        if let Some(raw) = ns_window_ptr(&win) {
            configure_ns_window(raw);
            POPUP_NSWIN.store(raw, Ordering::SeqCst);
        }
        Ok(())
    }

    fn popup_raw() -> Option<isize> {
        match POPUP_NSWIN.load(Ordering::SeqCst) {
            0 => None,
            p => Some(p),
        }
    }

    /// Pick the screen holding the focused application's active window (matching Windows'
    /// `MonitorFromWindow(GetForegroundWindow())`), falling back to the screen under the mouse
    /// cursor and then the main screen. Places the popup horizontally centred above the bottom edge
    /// (and above a bottom Dock if present).
    fn compute_frame(mtm: MainThreadMarker) -> Option<NSRect> {
        let screens = NSScreen::screens(mtm);
        // Primary screen height is used to flip CoreGraphics top-left Y into Cocoa bottom-left Y.
        let primary_h = screens
            .iter()
            .next()
            .map(|s| s.frame().size.height)
            .unwrap_or(0.0);
        let anchor = crate::watcher::ax_macos::focused_window_center()
            .filter(|_| primary_h > 0.0)
            .map(|(cg_x, cg_y)| NSPoint::new(cg_x, primary_h - cg_y))
            .unwrap_or_else(NSEvent::mouseLocation);

        let mut chosen = None;
        for screen in screens.iter() {
            let f = screen.frame();
            if anchor.x >= f.origin.x
                && anchor.x <= f.origin.x + f.size.width
                && anchor.y >= f.origin.y
                && anchor.y <= f.origin.y + f.size.height
            {
                chosen = Some(screen);
                break;
            }
        }
        let screen = chosen.or_else(|| NSScreen::mainScreen(mtm))?;
        let frame = screen.frame();
        let visible = screen.visibleFrame();
        let x = visible.origin.x + ((visible.size.width - POPUP_WINDOW_W) / 2.0).round();
        // Keep the pill cleanly above the Dock (visible.origin.y) or POPUP_BOTTOM_MARGIN above screen bottom.
        let y = visible.origin.y.max(frame.origin.y + POPUP_BOTTOM_MARGIN).round();
        Some(NSRect::new(
            NSPoint::new(x, y),
            NSSize::new(POPUP_WINDOW_W, POPUP_WINDOW_H),
        ))
    }

    /// On macOS, `tao` unconditionally calls `NSApp.activateIgnoringOtherApps(false)` at launch,
    /// which queues a one-time deferred WindowServer activation that fires the first time any
    /// window is ordered on screen. For `--hidden` / `--popup-check` launches, drain that pending
    /// launch activation invisibly (`alpha = 0`) at `RunEvent::Ready` and immediately return focus
    /// to whatever app was frontmost, so the very first dictation never steals focus.
    pub fn prime_hidden_launch(app: &AppHandle) {
        let Some(raw) = popup_raw() else { return };
        let ws = NSWorkspace::sharedWorkspace();
        let me = std::process::id() as i32;
        let prev_pid = ws
            .frontmostApplication()
            .map(|a| a.processIdentifier())
            .filter(|&p| p != 0 && p != me);

        let app_clone = app.clone();
        let _ = app.run_on_main_thread(move || unsafe {
            let win: &NSWindow = &*(raw as *const NSWindow);
            win.setAlphaValue(0.0);
            win.orderFrontRegardless();
            win.orderOut(None);
            win.setAlphaValue(1.0);
        });

        if let Some(pid) = prev_pid {
            std::thread::spawn(move || {
                // Wait briefly for WindowServer to deliver the deferred launch activation, then yield back.
                for _ in 0..10 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    let front = objc2::rc::autoreleasepool(|_| {
                        NSWorkspace::sharedWorkspace()
                            .frontmostApplication()
                            .map(|a| a.processIdentifier())
                            .unwrap_or(0)
                    });
                    if front == me {
                        let _ = app_clone.run_on_main_thread(move || {
                            if let Some(prev) = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
                                #[allow(deprecated)]
                                let _ = prev.activateWithOptions(
                                    objc2_app_kit::NSApplicationActivationOptions::ActivateIgnoringOtherApps,
                                );
                            }
                        });
                        break;
                    }
                }
            });
        }
    }

    pub fn show(app: &AppHandle) {
        let Some(raw) = popup_raw() else { return };
        let _ = app.run_on_main_thread(move || {
            let Some(mtm) = MainThreadMarker::new() else { return };
            unsafe {
                let win: &NSWindow = &*(raw as *const NSWindow);
                win.setLevel(NSStatusWindowLevel);
                if let Some(rect) = compute_frame(mtm) {
                    win.setFrame_display(rect, true);
                    *POPUP_FRAME.lock().unwrap_or_else(|e| e.into_inner()) = Some((
                        rect.origin.x,
                        rect.origin.y,
                        rect.origin.x + rect.size.width,
                        rect.origin.y + rect.size.height,
                    ));
                }
                // `orderFrontRegardless` on a NonactivatingPanel displays at NSStatusWindowLevel
                // above all spaces and fullscreen apps without activating Wren or stealing focus.
                win.orderFrontRegardless();
            }
            POPUP_VISIBLE.store(true, Ordering::SeqCst);
        });
    }

    pub fn hide(app: &AppHandle) {
        let Some(raw) = popup_raw() else { return };
        let _ = app.run_on_main_thread(move || {
            unsafe {
                let win: &NSWindow = &*(raw as *const NSWindow);
                win.orderOut(None);
            }
            POPUP_VISIBLE.store(false, Ordering::SeqCst);
        });
    }

    fn cursor_over_pill() -> bool {
        if !POPUP_VISIBLE.load(Ordering::SeqCst) {
            return false;
        }
        let Some((left, bottom, right, top)) = *POPUP_FRAME.lock().unwrap_or_else(|e| e.into_inner()) else {
            return false;
        };
        let mouse = objc2::rc::autoreleasepool(|_| NSEvent::mouseLocation());
        if mouse.x < left || mouse.x >= right || mouse.y < bottom || mouse.y >= top {
            return false;
        }
        // Convert Cocoa bottom-up coordinates into top-down logical pixels inside the window.
        let local_x = mouse.x - left;
        let local_y = top - mouse.y;
        let hit = *hit_rect_lock();
        cursor_over(
            (local_x.round() as i32, local_y.round() as i32),
            (0, 0, POPUP_WINDOW_W as i32, POPUP_WINDOW_H as i32),
            hit,
        )
    }

    fn post_click_style(app: &AppHandle, raw: isize, take_clicks: bool) {
        let _ = app.run_on_main_thread(move || {
            let take_clicks = take_clicks && CLICK_WANTED.load(Ordering::SeqCst);
            unsafe {
                let win: &NSWindow = &*(raw as *const NSWindow);
                win.setIgnoresMouseEvents(!take_clicks);
            }
        });
    }

    pub fn set_clickable(app: &AppHandle, clickable: bool) {
        let Some(raw) = popup_raw() else { return };
        CLICK_WANTED.store(clickable, Ordering::SeqCst);
        if !clickable {
            CLICK_APPLIED.store(false, Ordering::SeqCst);
            post_click_style(app, raw, false);
            return;
        }
        if TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
            return;
        }
        let thread_app = app.clone();
        let spawned = std::thread::Builder::new().name("wren-popup-hover".into()).spawn(move || {
            let poll = std::time::Duration::from_millis(POPUP_HOVER_POLL_MS);
            loop {
                if !CLICK_WANTED.load(Ordering::SeqCst) {
                    TRACKER_RUNNING.store(false, Ordering::SeqCst);
                    if CLICK_WANTED.load(Ordering::SeqCst) && !TRACKER_RUNNING.swap(true, Ordering::SeqCst) {
                        continue;
                    }
                    return;
                }
                let over = cursor_over_pill();
                if over != CLICK_APPLIED.load(Ordering::SeqCst) {
                    CLICK_APPLIED.store(over, Ordering::SeqCst);
                    post_click_style(&thread_app, raw, over);
                }
                std::thread::sleep(poll);
            }
        });
        if let Err(err) = spawned {
            TRACKER_RUNNING.store(false, Ordering::SeqCst);
            tracing::warn!(error = %err, "popup: cannot start hover thread; making whole window clickable");
            post_click_style(app, raw, true);
        }
    }

    pub fn is_own_window(_app: &AppHandle, win_id: isize) -> bool {
        // On macOS, `paste::foreground().hwnd` packs `(pid << 32) | (ax_window_hash & 0xFFFF_FFFF)`.
        let pid = ((win_id as u64) >> 32) as u32;
        pid != 0 && pid == std::process::id()
    }

    pub fn self_check(app: &AppHandle) -> Vec<String> {
        use std::time::Duration;
        let mut failures = Vec::new();
        let Some(raw) = popup_raw() else {
            return vec!["popup window not found".into()];
        };
        let settle = || std::thread::sleep(Duration::from_millis(300));
        let query = |f: fn(&NSWindow) -> bool| -> bool {
            let (tx, rx) = crossbeam_channel::bounded(1);
            let _ = app.run_on_main_thread(move || {
                let val = unsafe { f(&*(raw as *const NSWindow)) };
                let _ = tx.send(val);
            });
            rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
        };
        let front_pid = || -> i32 {
            NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|a| a.processIdentifier())
                .unwrap_or(0)
        };

        let pid_before = front_pid();
        let me = std::process::id() as i32;
        let check_front = |stage: &str, failures: &mut Vec<String>| {
            let now = front_pid();
            if pid_before != 0 && now != pid_before && now == me {
                failures.push(format!("{stage}: Wren became frontmost application (before={pid_before}, now={now})"));
            }
        };
        show(app);
        settle();
        check_front("after show", &mut failures);
        if !query(|w| w.isVisible()) {
            failures.push("after show: not visible".into());
        }
        if !query(|w| w.ignoresMouseEvents()) {
            failures.push("after show: not click-through".into());
        }
        if query(|w| w.isKeyWindow()) {
            failures.push("after show: popup stole key window focus".into());
        }

        let saved_rect = *hit_rect_lock();
        set_hit_rect(HitRect { x: 0.0, y: 0.0, w: POPUP_WINDOW_W, h: POPUP_WINDOW_H });
        set_clickable(app, true);
        settle();
        check_front("after set_clickable(true)", &mut failures);
        let over = cursor_over_pill();
        let takes_clicks = !query(|w| w.ignoresMouseEvents());
        if takes_clicks != over {
            failures.push(format!("set_clickable(true): takes clicks {takes_clicks}, cursor over pill {over}"));
        }
        *hit_rect_lock() = saved_rect;

        set_clickable(app, false);
        settle();
        check_front("after set_clickable(false)", &mut failures);
        if !query(|w| w.ignoresMouseEvents()) {
            failures.push("after set_clickable(false): not click-through".into());
        }

        hide(app);
        settle();
        check_front("after hide", &mut failures);
        if query(|w| w.isVisible()) {
            failures.push("after hide: still visible".into());
        }

        show(app);
        settle();
        check_front("after second show", &mut failures);
        if !query(|w| w.isVisible()) {
            failures.push("after second show: not visible".into());
        }
        hide(app);
        settle();
        check_front("after second hide", &mut failures);
        if query(|w| w.isVisible()) {
            failures.push("after second hide: still visible".into());
        }

        failures
    }
}

pub use platform::{create, hide, is_own_window, self_check, set_clickable, show};
#[cfg(target_os = "macos")]
pub use platform::prime_hidden_launch;

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
