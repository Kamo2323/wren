//! UI Automation (Windows) for the correction watcher and the pre-paste check: which element has
//! keyboard focus, what kind it is, and its text. Read-only: nothing here ever sets a value, moves
//! focus or sends input.
//!
//! Every function must be called on a thread that joined the COM multithreaded apartment
//! ([`ComMta`]); UI Automation clients on an MTA thread never need a message pump.
//!
//! Privacy: text read here is handed back to the caller and never logged.

use std::time::Instant;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, SAFEARRAY,
};
use windows::Win32::System::Ole::{SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_I4, VariantClear};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationTextPattern, IUIAutomationValuePattern, UIA_ClassNamePropertyId,
    UIA_ControlTypePropertyId, UIA_FrameworkIdPropertyId, UIA_IsKeyboardFocusablePropertyId,
    UIA_IsTextEditPatternAvailablePropertyId, UIA_IsTextPatternAvailablePropertyId,
    UIA_IsValuePatternAvailablePropertyId, UIA_PROPERTY_ID, UIA_ProcessIdPropertyId, UIA_TextPatternId,
    UIA_ValueIsReadOnlyPropertyId, UIA_ValuePatternId,
};
use windows::core::Interface;

/// Joins the COM multithreaded apartment for the life of the value.
pub struct ComMta {
    joined: bool,
}

impl ComMta {
    pub fn join() -> Self {
        // S_FALSE (already joined) also needs a matching CoUninitialize; RPC_E_CHANGED_MODE (an STA
        // thread) does not, and UI Automation still works there for our synchronous calls.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        Self { joined: hr.is_ok() }
    }
}

impl Drop for ComMta {
    fn drop(&mut self) {
        if self.joined {
            unsafe { CoUninitialize() };
        }
    }
}

/// What UI Automation says about the focused element (no text).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusFacts {
    pub control_type: i32,
    pub pid: u32,
    pub value_pattern: bool,
    /// Only meaningful when `value_pattern` is set.
    pub value_read_only: bool,
    pub text_pattern: bool,
    pub text_edit_pattern: bool,
    pub keyboard_focusable: bool,
    pub class_name: String,
    pub framework: String,
}

/// The focused element, its facts and its runtime id (identity while it lives).
pub struct Focused {
    element: IUIAutomationElement,
    pub facts: FocusFacts,
    pub runtime_id: Vec<i32>,
}

/// One UI Automation client.
pub struct Uia {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
}

const CACHED: [UIA_PROPERTY_ID; 9] = [
    UIA_ControlTypePropertyId,
    UIA_ProcessIdPropertyId,
    UIA_IsValuePatternAvailablePropertyId,
    UIA_ValueIsReadOnlyPropertyId,
    UIA_IsTextPatternAvailablePropertyId,
    UIA_IsTextEditPatternAvailablePropertyId,
    UIA_IsKeyboardFocusablePropertyId,
    UIA_ClassNamePropertyId,
    UIA_FrameworkIdPropertyId,
];

impl Uia {
    /// A client whose cross-process requests are abandoned after `timeout_ms` (the caller must
    /// have joined the MTA).
    pub fn new(timeout_ms: u64) -> windows::core::Result<Self> {
        let automation: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
        if let Ok(a2) = automation.cast::<IUIAutomation2>() {
            let t = timeout_ms.min(u32::MAX as u64) as u32;
            unsafe {
                let _ = a2.SetConnectionTimeout(t);
                let _ = a2.SetTransactionTimeout(t);
            }
        }
        let cache = unsafe { automation.CreateCacheRequest()? };
        for p in CACHED {
            unsafe { cache.AddProperty(p)? };
        }
        Ok(Self { automation, cache })
    }

    pub fn automation(&self) -> &IUIAutomation {
        &self.automation
    }

    /// The element with keyboard focus, with its facts fetched in one round trip.
    pub fn focused(&self) -> windows::core::Result<Focused> {
        let element = unsafe { self.automation.GetFocusedElementBuildCache(&self.cache)? };
        Ok(self.wrap(element))
    }

    /// `element` with its facts (fetched now, one round trip).
    pub fn facts_of(&self, element: &IUIAutomationElement) -> windows::core::Result<Focused> {
        let element = unsafe { element.BuildUpdatedCache(&self.cache)? };
        Ok(self.wrap(element))
    }

    fn wrap(&self, element: IUIAutomationElement) -> Focused {
        let facts = FocusFacts {
            control_type: cached_i32(&element, UIA_ControlTypePropertyId).unwrap_or(0),
            pid: cached_i32(&element, UIA_ProcessIdPropertyId).unwrap_or(0) as u32,
            value_pattern: cached_bool(&element, UIA_IsValuePatternAvailablePropertyId),
            value_read_only: cached_bool(&element, UIA_ValueIsReadOnlyPropertyId),
            text_pattern: cached_bool(&element, UIA_IsTextPatternAvailablePropertyId),
            text_edit_pattern: cached_bool(&element, UIA_IsTextEditPatternAvailablePropertyId),
            keyboard_focusable: cached_bool(&element, UIA_IsKeyboardFocusablePropertyId),
            class_name: unsafe { element.CachedClassName() }.map(|b| b.to_string()).unwrap_or_default(),
            framework: unsafe { element.CachedFrameworkId() }.map(|b| b.to_string()).unwrap_or_default(),
        };
        let runtime_id = unsafe { element.GetRuntimeId() }.map(safearray_i32).unwrap_or_default();
        Focused { element, facts, runtime_id }
    }

    /// The element's whole text: TextPattern `DocumentRange.GetText(-1)`, else ValuePattern's
    /// value. `Ok(None)` when it offers neither.
    pub fn read_text(&self, f: &Focused) -> windows::core::Result<Option<String>> {
        if f.facts.text_pattern {
            match unsafe { f.element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) } {
                Ok(tp) => {
                    let range = unsafe { tp.DocumentRange()? };
                    let text = unsafe { range.GetText(-1)? };
                    return Ok(Some(text.to_string()));
                }
                Err(_) if f.facts.value_pattern => {}
                Err(e) => return Err(e),
            }
        }
        if f.facts.value_pattern {
            let vp = unsafe { f.element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)? };
            let v = unsafe { vp.CurrentValue()? };
            return Ok(Some(v.to_string()));
        }
        Ok(None)
    }

    /// Times one closure in milliseconds.
    pub fn timed<T>(f: impl FnOnce() -> T) -> (T, u64) {
        let t = Instant::now();
        let out = f();
        (out, t.elapsed().as_millis() as u64)
    }
}

impl Focused {
    pub fn element(&self) -> &IUIAutomationElement {
        &self.element
    }
}

fn cached_variant(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<VARIANT> {
    unsafe { el.GetCachedPropertyValue(id) }.ok()
}

fn cached_bool(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> bool {
    let Some(mut v) = cached_variant(el, id) else { return false };
    let out = unsafe {
        let inner = &v.Anonymous.Anonymous;
        inner.vt == VT_BOOL && inner.Anonymous.boolVal.as_bool()
    };
    unsafe {
        let _ = VariantClear(&mut v);
    }
    out
}

fn cached_i32(el: &IUIAutomationElement, id: UIA_PROPERTY_ID) -> Option<i32> {
    let mut v = cached_variant(el, id)?;
    let out = unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_I4).then(|| inner.Anonymous.lVal)
    };
    unsafe {
        let _ = VariantClear(&mut v);
    }
    out
}

/// Copies a one-dimensional SAFEARRAY of i32 and frees it.
fn safearray_i32(sa: *mut SAFEARRAY) -> Vec<i32> {
    if sa.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    unsafe {
        if let (Ok(lo), Ok(hi)) = (SafeArrayGetLBound(sa, 1), SafeArrayGetUBound(sa, 1)) {
            for i in lo..=hi {
                let mut v = 0i32;
                if SafeArrayGetElement(sa, &i, &mut v as *mut i32 as *mut _).is_ok() {
                    out.push(v);
                }
            }
        }
        let _ = SafeArrayDestroy(sa);
    }
    out
}

/// Readable names for the control types that matter to Wren (for logs and the probe report).
pub fn control_type_name(id: i32) -> &'static str {
    use windows::Win32::UI::Accessibility::*;
    let id = UIA_CONTROLTYPE_ID(id);
    match id {
        x if x == UIA_EditControlTypeId => "Edit",
        x if x == UIA_DocumentControlTypeId => "Document",
        x if x == UIA_ComboBoxControlTypeId => "ComboBox",
        x if x == UIA_PaneControlTypeId => "Pane",
        x if x == UIA_WindowControlTypeId => "Window",
        x if x == UIA_CustomControlTypeId => "Custom",
        x if x == UIA_GroupControlTypeId => "Group",
        x if x == UIA_TextControlTypeId => "Text",
        x if x == UIA_ButtonControlTypeId => "Button",
        x if x == UIA_HyperlinkControlTypeId => "Hyperlink",
        x if x == UIA_ListControlTypeId => "List",
        x if x == UIA_ListItemControlTypeId => "ListItem",
        x if x == UIA_TreeItemControlTypeId => "TreeItem",
        x if x == UIA_MenuItemControlTypeId => "MenuItem",
        x if x == UIA_TabItemControlTypeId => "TabItem",
        x if x == UIA_ImageControlTypeId => "Image",
        x if x == UIA_CheckBoxControlTypeId => "CheckBox",
        x if x == UIA_DataItemControlTypeId => "DataItem",
        _ => "Other",
    }
}
