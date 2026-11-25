//! Windows reader for the Teams meeting window (plan Task 4): finds new Teams' top-level windows
//! (`ms-teams.exe`) and copies the meeting window's UI Automation tree (raw view, cached properties).
//! Read-only: nothing here invokes, focuses or types.
//!
//! Must run on a thread that joined the COM MTA (`crate::watcher::uia::ComMta`).

use super::snapshot::UiNode;
use super::{is_meeting_window, TeamsReader, MAX_DEPTH, MAX_NODES, SNAPSHOT_BUDGET};
use std::time::Instant;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationElement,
    IUIAutomationTreeWalker, UIA_AutomationIdPropertyId, UIA_ClassNamePropertyId, UIA_ControlTypePropertyId,
    UIA_HelpTextPropertyId, UIA_NamePropertyId, UIA_ToggleToggleStatePropertyId,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible};
use windows::core::Interface;

/// Cross-process UIA calls give up after this long (Teams can hang while busy).
const UIA_TIMEOUT_MS: u32 = 400;

fn role_name(ct: i32) -> &'static str {
    match ct {
        50000 => "Button",
        50002 => "CheckBox",
        50003 => "ComboBox",
        50004 => "Edit",
        50005 => "Hyperlink",
        50006 => "Image",
        50007 => "ListItem",
        50008 => "List",
        50009 => "Menu",
        50011 => "MenuItem",
        50013 => "RadioButton",
        50018 => "Tab",
        50019 => "TabItem",
        50020 => "Text",
        50021 => "ToolBar",
        50022 => "ToolTip",
        50025 => "Custom",
        50026 => "Group",
        50030 => "Document",
        50031 => "SplitButton",
        50032 => "Window",
        50033 => "Pane",
        50034 => "Header",
        50037 => "TitleBar",
        _ => "Other",
    }
}

pub struct UiaTeamsReader {
    automation: IUIAutomation,
    cache: IUIAutomationCacheRequest,
    walker: IUIAutomationTreeWalker,
    /// Dump the first Teams window even when it isn't a meeting (probe `--any`).
    pub any_window: bool,
}

impl UiaTeamsReader {
    pub fn new() -> windows::core::Result<Self> {
        let automation: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)? };
        if let Ok(a2) = automation.cast::<IUIAutomation2>() {
            unsafe {
                let _ = a2.SetConnectionTimeout(UIA_TIMEOUT_MS);
                let _ = a2.SetTransactionTimeout(UIA_TIMEOUT_MS);
            }
        }
        let cache = unsafe { automation.CreateCacheRequest()? };
        for p in [
            UIA_NamePropertyId,
            UIA_HelpTextPropertyId,
            UIA_AutomationIdPropertyId,
            UIA_ClassNamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_ToggleToggleStatePropertyId,
        ] {
            unsafe { cache.AddProperty(p)? };
        }
        let walker = unsafe { automation.RawViewWalker()? };
        Ok(Self { automation, cache, walker, any_window: false })
    }

    fn node(&self, el: &IUIAutomationElement) -> UiNode {
        unsafe {
            let s = |r: windows::core::Result<windows::core::BSTR>| r.map(|b| b.to_string()).unwrap_or_default();
            let ct = el.CachedControlType().map(|c| c.0).unwrap_or(0);
            let mut states = Vec::new();
            if let Ok(mut v) = el.GetCachedPropertyValue(UIA_ToggleToggleStatePropertyId) {
                if let Some(t) = variant_i32(&mut v) {
                    states.push(if t == 1 { "toggled_on" } else if t == 0 { "toggled_off" } else { "toggled_mixed" }.into());
                }
            }
            UiNode {
                role: role_name(ct).into(),
                name: s(el.CachedName()),
                help: s(el.CachedHelpText()),
                value: String::new(),
                automation_id: s(el.CachedAutomationId()),
                class: s(el.CachedClassName()),
                states,
                children: Vec::new(),
            }
        }
    }

    /// Copy the subtree under `el` within the depth / node / time limits.
    fn copy(&self, el: &IUIAutomationElement, depth: usize, count: &mut usize, deadline: Instant) -> UiNode {
        let mut node = self.node(el);
        *count += 1;
        if depth >= MAX_DEPTH {
            return node;
        }
        let mut child = unsafe { self.walker.GetFirstChildElementBuildCache(el, &self.cache).ok() };
        while let Some(c) = child {
            if *count >= MAX_NODES || Instant::now() > deadline {
                break;
            }
            node.children.push(self.copy(&c, depth + 1, count, deadline));
            child = unsafe { self.walker.GetNextSiblingElementBuildCache(&c, &self.cache).ok() };
        }
        node
    }

    fn tree_of(&self, hwnd: HWND) -> Option<UiNode> {
        let el = unsafe { self.automation.ElementFromHandleBuildCache(hwnd, &self.cache).ok()? };
        let mut count = 0;
        // Teams' tree is big; allow a few budgets for one window, the caller polls again anyway.
        Some(self.copy(&el, 0, &mut count, Instant::now() + SNAPSHOT_BUDGET * 2))
    }
}

fn variant_i32(v: &mut VARIANT) -> Option<i32> {
    let out = unsafe {
        let inner = &v.Anonymous.Anonymous;
        (inner.vt == VT_I4).then(|| inner.Anonymous.lVal)
    };
    unsafe {
        let _ = VariantClear(v);
    }
    out
}

/// Visible top-level windows of `ms-teams.exe`, with titles.
pub fn teams_windows() -> Vec<(HWND, String)> {
    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        let out = unsafe { &mut *(lp.0 as *mut Vec<(HWND, String)>) };
        if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
            return BOOL(1);
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        let is_teams = crate::engine::process_image(pid)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_ascii_lowercase()))
            .is_some_and(|n| n == "ms-teams.exe");
        if is_teams {
            let mut buf = [0u16; 512];
            let len = unsafe { GetWindowTextW(hwnd, &mut buf) }.max(0) as usize;
            out.push((hwnd, String::from_utf16_lossy(&buf[..len])));
        }
        BOOL(1)
    }
    let mut out: Vec<(HWND, String)> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

impl TeamsReader for UiaTeamsReader {
    fn snapshot(&mut self) -> Option<(UiNode, String)> {
        let windows = teams_windows();
        // Meeting windows usually aren't titled just "Microsoft Teams"; try likely ones first.
        let mut ordered = windows.clone();
        ordered.sort_by_key(|(_, t)| t.trim().eq_ignore_ascii_case("Microsoft Teams") as u8);
        for (hwnd, title) in ordered {
            let Some(tree) = self.tree_of(hwnd) else { continue };
            if self.any_window || is_meeting_window(&tree) {
                return Some((tree, title));
            }
        }
        None
    }
}
