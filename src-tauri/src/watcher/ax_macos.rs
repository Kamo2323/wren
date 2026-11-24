//! macOS Accessibility (`AXUIElement`) counterpart to Windows UI Automation (`uia.rs`).
//!
//! Read-only except for setting `AXManualAccessibility = true` on Electron/Chromium apps during
//! `prime()` so their accessibility trees expose text boxes (matching how Windows UI Automation
//! wakes Chromium's accessibility tree on first query).
//!
//! Privacy: text read here is handed back to the caller and never logged.

use std::ffi::{CStr, CString, c_char, c_void};

type CFTypeRef = *const c_void;
type CFStringRef = *const c_void;
type CFBooleanRef = *const c_void;
type CFNumberRef = *const c_void;
type AXUIElementRef = *const c_void;
type AXValueRef = *const c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_NUMBER_SINT64_TYPE: isize = 4;
const K_AX_VALUE_CGPOINT_TYPE: u32 = 1;
const K_AX_VALUE_CGSIZE_TYPE: u32 = 2;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CGSize {
    width: f64,
    height: f64,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        element: AXUIElementRef,
        attribute: CFStringRef,
        settable: *mut u8,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_in_seconds: f32) -> AXError;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXValueGetValue(value: AXValueRef, value_type: u32, value_ptr: *mut c_void) -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFBooleanRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFHash(cf: CFTypeRef) -> usize;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFNumberGetValue(number: CFNumberRef, the_type: isize, value_ptr: *mut c_void) -> u8;
    fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> CFStringRef;
    fn CFStringGetLength(the_string: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
}

/// Owned `CFTypeRef` released on drop.
pub(crate) struct CfGuard(CFTypeRef);

impl CfGuard {
    pub(crate) fn new(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }
    pub(crate) fn as_ptr(&self) -> CFTypeRef {
        self.0
    }
}

impl Drop for CfGuard {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

pub(crate) fn cf_str(s: &str) -> Option<CfGuard> {
    let cs = CString::new(s).ok()?;
    let ptr = unsafe { CFStringCreateWithCString(std::ptr::null(), cs.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
    CfGuard::new(ptr)
}

pub(crate) fn cf_to_string(ptr: CFTypeRef) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    unsafe {
        if CFGetTypeID(ptr) != CFStringGetTypeID() {
            return None;
        }
        let len = CFStringGetLength(ptr);
        let max = CFStringGetMaximumSizeForEncoding(len, K_CF_STRING_ENCODING_UTF8) + 1;
        if max <= 0 {
            return Some(String::new());
        }
        let mut buf = vec![0u8; max as usize];
        if CFStringGetCString(ptr, buf.as_mut_ptr() as *mut c_char, max, K_CF_STRING_ENCODING_UTF8) == 0 {
            return None;
        }
        CStr::from_ptr(buf.as_ptr() as *const c_char)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

pub(crate) fn cf_to_i64(ptr: CFTypeRef) -> Option<i64> {
    if ptr.is_null() {
        return None;
    }
    unsafe {
        if CFGetTypeID(ptr) != CFNumberGetTypeID() {
            return None;
        }
        let mut out: i64 = 0;
        if CFNumberGetValue(ptr, K_CF_NUMBER_SINT64_TYPE, (&mut out as *mut i64) as *mut c_void) != 0 {
            Some(out)
        } else {
            None
        }
    }
}

pub(crate) fn copy_attr(elem: AXUIElementRef, attr: &str) -> Option<CfGuard> {
    let key = cf_str(attr)?;
    let mut val: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(elem, key.as_ptr(), &mut val) };
    if err != K_AX_ERROR_SUCCESS {
        return None;
    }
    CfGuard::new(val)
}

fn attr_string(elem: AXUIElementRef, attr: &str) -> Option<String> {
    let val = copy_attr(elem, attr)?;
    cf_to_string(val.as_ptr())
}

fn attr_i64(elem: AXUIElementRef, attr: &str) -> Option<i64> {
    let val = copy_attr(elem, attr)?;
    cf_to_i64(val.as_ptr())
}

fn attr_settable(elem: AXUIElementRef, attr: &str) -> bool {
    let Some(key) = cf_str(attr) else { return false };
    let mut settable: u8 = 0;
    let err = unsafe { AXUIElementIsAttributeSettable(elem, key.as_ptr(), &mut settable) };
    err == K_AX_ERROR_SUCCESS && settable != 0
}

/// Stable-ish 32-bit hash token for the currently focused window of `pid` (used by `paste::foreground`
/// to notice when the user switched windows inside the same app).
pub fn focused_window_token(pid: u32) -> Option<u32> {
    if pid == 0 || !crate::hotkey::accessibility_trusted(false) {
        return None;
    }
    let app = CfGuard::new(unsafe { AXUIElementCreateApplication(pid as i32) })?;
    unsafe { AXUIElementSetMessagingTimeout(app.as_ptr(), 0.05) };
    let win = copy_attr(app.as_ptr(), "AXFocusedWindow")?;
    let hash = unsafe { CFHash(win.as_ptr()) };
    Some((hash as u32) ^ ((hash >> 32) as u32))
}

/// Center point `(x, y)` of the frontmost app's focused window in CoreGraphics top-left screen
/// coordinates, used by `popup` to appear on the display holding the active window.
pub fn focused_window_center() -> Option<(f64, f64)> {
    if !crate::hotkey::accessibility_trusted(false) {
        return None;
    }
    let pid = objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|a| a.processIdentifier())
            .unwrap_or(0)
    });
    if pid <= 0 || pid == std::process::id() as i32 {
        return None;
    }
    let app = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) })?;
    unsafe { AXUIElementSetMessagingTimeout(app.as_ptr(), 0.04) };
    let win = copy_attr(app.as_ptr(), "AXFocusedWindow")?;
    let pos_val = copy_attr(win.as_ptr(), "AXPosition")?;
    let size_val = copy_attr(win.as_ptr(), "AXSize")?;
    let mut pos = CGPoint::default();
    let mut size = CGSize::default();
    let ok_pos = unsafe {
        AXValueGetValue(
            pos_val.as_ptr(),
            K_AX_VALUE_CGPOINT_TYPE,
            (&mut pos as *mut CGPoint) as *mut c_void,
        ) != 0
    };
    let ok_size = unsafe {
        AXValueGetValue(
            size_val.as_ptr(),
            K_AX_VALUE_CGSIZE_TYPE,
            (&mut size as *mut CGSize) as *mut c_void,
        ) != 0
    };
    if ok_pos && ok_size && size.width > 0.0 && size.height > 0.0 {
        Some((pos.x + size.width / 2.0, pos.y + size.height / 2.0))
    } else {
        None
    }
}

/// Wake Chromium/Electron apps' accessibility trees (`AXManualAccessibility = true`) when a
/// recording starts so pre-paste checks and the correction watcher see actual text inputs.
pub fn prime_frontmost() {
    if !crate::hotkey::accessibility_trusted(false) {
        return;
    }
    let pid = objc2::rc::autoreleasepool(|_| {
        objc2_app_kit::NSWorkspace::sharedWorkspace()
            .frontmostApplication()
            .map(|a| a.processIdentifier())
            .unwrap_or(0)
    });
    if pid <= 0 {
        return;
    }
    let Some(ax_app) = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) }) else {
        return;
    };
    unsafe { AXUIElementSetMessagingTimeout(ax_app.as_ptr(), 0.05) };
    if let Some(key) = cf_str("AXManualAccessibility") {
        unsafe {
            let _ = AXUIElementSetAttributeValue(ax_app.as_ptr(), key.as_ptr(), kCFBooleanTrue as CFTypeRef);
        }
    }
    // Also do a single lightweight focused-element read to ensure the tree is hydrated.
    let _ = copy_attr(ax_app.as_ptr(), "AXFocusedUIElement");
}

/// What macOS Accessibility reports about the focused element (no text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusFacts {
    pub pid: u32,
    pub role: String,
    pub subrole: String,
    pub value_settable: bool,
    pub has_value: bool,
}

/// A live focused Accessibility element, its facts, and an identity key.
pub struct Focused {
    element: CfGuard,
    pub facts: FocusFacts,
    pub runtime_id: Vec<i32>,
}

/// One macOS Accessibility client (mirrors `uia::Uia`).
pub struct AxClient {
    system: CfGuard,
    timeout_secs: f32,
}

impl AxClient {
    pub fn new(timeout_ms: u64) -> anyhow::Result<Self> {
        let system = CfGuard::new(unsafe { AXUIElementCreateSystemWide() })
            .ok_or_else(|| anyhow::anyhow!("AXUIElementCreateSystemWide returned null"))?;
        let timeout_secs = (timeout_ms as f32 / 1000.0).max(0.02);
        unsafe { AXUIElementSetMessagingTimeout(system.as_ptr(), timeout_secs) };
        Ok(Self { system, timeout_secs })
    }

    pub fn focused(&self) -> anyhow::Result<Focused> {
        let el = copy_attr(self.system.as_ptr(), "AXFocusedUIElement")
            .ok_or_else(|| anyhow::anyhow!("no AXFocusedUIElement"))?;
        unsafe { AXUIElementSetMessagingTimeout(el.as_ptr(), self.timeout_secs) };
        let mut pid = 0i32;
        unsafe { AXUIElementGetPid(el.as_ptr(), &mut pid) };
        let role = attr_string(el.as_ptr(), "AXRole").unwrap_or_default();
        let subrole = attr_string(el.as_ptr(), "AXSubrole").unwrap_or_default();
        let value_settable = attr_settable(el.as_ptr(), "AXValue");
        let has_value = value_settable || copy_attr(el.as_ptr(), "AXValue").is_some();
        let hash = unsafe { CFHash(el.as_ptr()) };
        let runtime_id = vec![pid, (hash & 0xFFFF_FFFF) as i32, ((hash >> 32) & 0xFFFF_FFFF) as i32];
        Ok(Focused {
            element: el,
            facts: FocusFacts {
                pid: pid.max(0) as u32,
                role,
                subrole,
                value_settable,
                has_value,
            },
            runtime_id,
        })
    }

    /// Read the element's string value (`AXValue`), bounded to `max_chars + 1`.
    /// Checks `AXNumberOfCharacters` first so huge documents (IDEs, Notes, Obsidian) never pay
    /// for a multi-megabyte Accessibility IPC copy before the tracker skips them as too large.
    pub fn read_text(&self, f: &Focused, max_chars: Option<usize>) -> anyhow::Result<Option<String>> {
        if let Some(m) = max_chars {
            let cap = m.saturating_add(1);
            if let Some(n) = attr_i64(f.element.as_ptr(), "AXNumberOfCharacters")
                && n > cap as i64
            {
                // Return a marker string of length `cap` so `track::watch` recognizes the box as
                // too large and stops watching without copying the whole document across IPC.
                return Ok(Some(" ".repeat(cap)));
            }
        }
        let Some(text) = attr_string(f.element.as_ptr(), "AXValue") else {
            return Ok(None);
        };
        let limit = max_chars.map(|m| m.saturating_add(1));
        let cut = match limit {
            Some(n) if text.chars().count() > n => text.chars().take(n).collect(),
            _ => text,
        };
        Ok(Some(cut))
    }
}
