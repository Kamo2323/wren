//! The recording popup window (spec §11 "Popup").
//!
//! The popup must NEVER take keyboard focus: if it did, Ctrl+V would land in Wren instead of the
//! app being dictated into. So it is created once at startup (hidden), made non-activating with
//! Win32 extended styles, made click-through, and shown only with `SW_SHOWNOACTIVATE`.
//! Never call Tauri's `show()` or `set_focus()` on it.

use crate::constants::{POPUP_BOTTOM_MARGIN, POPUP_WINDOW_H, POPUP_WINDOW_W};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST,
};

pub const POPUP_LABEL: &str = "popup";
/// Event the popup front end listens to for state changes.
pub const POPUP_STATE_EVENT: &str = "popup://state";
/// Event carrying mic level (0..1) about 30 times a second while listening.
pub const POPUP_LEVEL_EVENT: &str = "popup://level";

/// Create the popup window, hidden. Call once from `setup`.
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
    // Click-through first: Tauri rewrites the extended styles when it changes cursor handling,
    // so the non-activating styles must be applied after it (and again in `set_clickable`).
    let _ = win.set_ignore_cursor_events(true);
    make_non_activating(&win);
    Ok(())
}

fn hwnd_of(win: &WebviewWindow) -> Option<HWND> {
    // Go through the raw pointer so a windows-crate version difference with Tauri cannot bite.
    win.hwnd().ok().map(|h| HWND(h.0 as *mut core::ffi::c_void))
}

fn make_non_activating(win: &WebviewWindow) {
    let Some(hwnd) = hwnd_of(win) else { return };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let wanted = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
        // APPWINDOW would put it in Alt+Tab; TOOLWINDOW keeps it out.
        let unwanted = WS_EX_APPWINDOW.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, (ex | wanted) & !unwanted);
    }
}

/// Show the popup centred above the bottom of the monitor that holds the foreground window,
/// without activating it. Safe to call repeatedly.
pub fn show(app: &AppHandle) {
    let Some(win) = app.get_webview_window(POPUP_LABEL) else { return };
    let Some(hwnd) = hwnd_of(&win) else { return };
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
            let y = work.bottom - h - (POPUP_BOTTOM_MARGIN * scale).round() as i32;
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
}

/// Hide the popup (hiding never steals focus, so Tauri's hide is fine).
pub fn hide(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(POPUP_LABEL) {
        let _ = win.hide();
    }
}

/// Let the popup receive clicks (for "click to copy", "Undo", "×") or pass them through.
pub fn set_clickable(app: &AppHandle, clickable: bool) {
    if let Some(win) = app.get_webview_window(POPUP_LABEL) {
        let _ = win.set_ignore_cursor_events(!clickable);
        make_non_activating(&win);
    }
}

/// True if `hwnd` is one of Wren's own windows (used to refuse pasting into ourselves).
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
