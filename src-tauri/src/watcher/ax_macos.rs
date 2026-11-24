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
type AXUIElementRef = *const c_void;
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;
const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

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
        settable: *mut bool,
    ) -> AXError;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_in_seconds: f32) -> AXError;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFBooleanRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFHash(cf: CFTypeRef) -> usize;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const c_char, encoding: u32) -> CFStringRef;
    fn CFStringGetLength(the_string: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFStringGetCString(
        the_string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> bool;
}

/// Owned `CFTypeRef` released on drop.
struct CfGuard(CFTypeRef);

impl CfGuard {
    fn new(ptr: CFTypeRef) -> Option<Self> {
        (!ptr.is_null()).then_some(Self(ptr))
    }
    fn as_ptr(&self) -> CFTypeRef {
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

fn cf_str(s: &str) -> Option<CfGuard> {
    let cs = CString::new(s).ok()?;
    let ptr = unsafe { CFStringCreateWithCString(std::ptr::null(), cs.as_ptr(), K_CF_STRING_ENCODING_UTF8) };
    CfGuard::new(ptr)
}

fn cf_to_string(ptr: CFTypeRef) -> Option<String> {
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
        if !CFStringGetCString(ptr, buf.as_mut_ptr() as *mut c_char, max, K_CF_STRING_ENCODING_UTF8) {
            return None;
        }
        CStr::from_ptr(buf.as_ptr() as *const c_char)
            .to_str()
            .ok()
            .map(str::to_owned)
    }
}

fn copy_attr(elem: AXUIElementRef, attr: &str) -> Option<CfGuard> {
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

fn attr_settable(elem: AXUIElementRef, attr: &str) -> bool {
    let Some(key) = cf_str(attr) else { return false };
    let mut settable = false;
    let err = unsafe { AXUIElementIsAttributeSettable(elem, key.as_ptr(), &mut settable) };
    err == K_AX_ERROR_SUCCESS && settable
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

/// Wake Chromium/Electron apps' accessibility trees (`AXManualAccessibility = true`) when a
/// recording starts so pre-paste checks and the correction watcher see actual text inputs.
pub fn prime_frontmost() {
    if !crate::hotkey::accessibility_trusted(false) {
        return;
    }
    let Some(app) = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return;
    };
    let pid = app.processIdentifier();
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
    pub fn read_text(&self, f: &Focused, max_chars: Option<usize>) -> anyhow::Result<Option<String>> {
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
