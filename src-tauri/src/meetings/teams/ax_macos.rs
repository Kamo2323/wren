//! macOS reader for the Teams meeting window (plan Task 6), the counterpart of `uia.rs`: finds the
//! running Teams app(s) and copies the meeting window's Accessibility (AX) tree into a [`UiNode`].
//! Read-only except for setting `AXManualAccessibility` / `AXEnhancedUserInterface` on the Teams app
//! element when its tree comes back empty (Chromium/Electron only build the tree when asked).
//!
//! Needs the Accessibility permission (the same one dictation uses). Names and titles read here are
//! never logged.

use super::snapshot::UiNode;
use super::{is_meeting_window, TeamsReader, MAX_DEPTH, MAX_NODES, SNAPSHOT_BUDGET};
use crate::meetings::capture::mac_tap::is_teams_bundle;
use crate::watcher::ax_macos::{cf_str, cf_to_i64, cf_to_string, copy_attr, CfGuard};
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};
use std::collections::HashSet;
use std::ffi::c_void;
use std::time::{Duration, Instant};

type CFTypeRef = *const c_void;
type CFArrayRef = *const c_void;
type AXUIElementRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, timeout_in_seconds: f32) -> i32;
    fn AXUIElementSetAttributeValue(element: AXUIElementRef, attribute: CFTypeRef, value: CFTypeRef) -> i32;
    fn AXUIElementCopyMultipleAttributeValues(
        element: AXUIElementRef,
        attributes: CFArrayRef,
        options: u32,
        values: *mut CFArrayRef,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: isize) -> CFTypeRef;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
}

/// Cross-process AX calls give up after this long (Teams can hang while busy).
const AX_TIMEOUT_SECS: f32 = 0.4;
/// A window tree smaller than this hasn't been built yet: wake Chromium's accessibility.
const HYDRATE_BELOW: usize = 20;
/// How long Chromium takes to build its tree after being woken.
const HYDRATE_WAIT: Duration = Duration::from_millis(600);
/// Longest `value` kept (text boxes can hold whole messages; the parser never needs them).
const MAX_VALUE_CHARS: usize = 200;

/// The attributes copied per element, in one IPC round trip (order matters: see [`AxAttrs`]).
const ATTRS: [&str; 10] = [
    "AXRole",
    "AXTitle",
    "AXDescription",
    "AXHelp",
    "AXValue",
    "AXIdentifier",
    "AXDOMIdentifier",
    "AXDOMClassList",
    "AXSelected",
    "AXChildren",
];

/// An `AXValue`: text, a number (toggles, sliders) or a boolean.
#[derive(Debug, Clone, PartialEq)]
pub enum AxValue {
    Text(String),
    Number(i64),
    Bool(bool),
}

/// The raw attributes of one element (pure input to [`to_node`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AxAttrs {
    pub role: String,
    pub title: String,
    pub description: String,
    pub help: String,
    pub value: Option<AxValue>,
    pub identifier: String,
    pub dom_identifier: String,
    pub dom_classes: Vec<String>,
    pub selected: bool,
}

const TOGGLE_ROLES: &[&str] = &["AXCheckBox", "AXToggle", "AXSwitch"];

/// Map AX attributes onto the platform-neutral node: AXTitle (or AXDescription when there's no
/// title) → name, AXHelp (or the description when the title took `name`) → help, AXIdentifier (or
/// Chromium's AXDOMIdentifier, the HTML id, which is what UIA's AutomationId carries) → automation_id,
/// AXDOMClassList → class, toggles' 0/1/2 value → `toggled_off/on/mixed`, AXSelected → `selected`.
pub fn to_node(a: AxAttrs) -> UiNode {
    let title = a.title.trim().to_string();
    let description = a.description.trim().to_string();
    let (name, help) = if title.is_empty() {
        (description, a.help)
    } else if a.help.trim().is_empty() && !description.is_empty() && description != title {
        (title, description)
    } else {
        (title, a.help)
    };
    let mut states = Vec::new();
    let value = match a.value {
        Some(AxValue::Text(t)) => t.chars().take(MAX_VALUE_CHARS).collect(),
        Some(AxValue::Number(n)) => {
            if TOGGLE_ROLES.contains(&a.role.as_str()) {
                states.push(match n {
                    0 => "toggled_off",
                    1 => "toggled_on",
                    _ => "toggled_mixed",
                }
                .to_string());
            }
            n.to_string()
        }
        Some(AxValue::Bool(b)) => {
            if TOGGLE_ROLES.contains(&a.role.as_str()) {
                states.push(if b { "toggled_on" } else { "toggled_off" }.to_string());
            }
            b.to_string()
        }
        None => String::new(),
    };
    if a.selected {
        states.push("selected".into());
    }
    let automation_id = if a.identifier.is_empty() { a.dom_identifier } else { a.identifier };
    UiNode {
        role: a.role,
        name,
        help,
        value,
        automation_id,
        class: a.dom_classes.join(" "),
        states,
        children: Vec::new(),
    }
}

fn is_array(v: CFTypeRef) -> bool {
    !v.is_null() && unsafe { CFGetTypeID(v) == CFArrayGetTypeID() }
}

/// Items of a CFArray (borrowed: valid while the array is alive).
fn array_items(arr: CFArrayRef) -> Vec<CFTypeRef> {
    if !is_array(arr) {
        return Vec::new();
    }
    let n = unsafe { CFArrayGetCount(arr) }.max(0);
    (0..n).map(|i| unsafe { CFArrayGetValueAtIndex(arr, i) }).filter(|p| !p.is_null()).collect()
}

fn ax_value(v: CFTypeRef) -> Option<AxValue> {
    if v.is_null() {
        return None;
    }
    if let Some(s) = cf_to_string(v) {
        return Some(AxValue::Text(s));
    }
    if unsafe { CFGetTypeID(v) == CFBooleanGetTypeID() } {
        return Some(AxValue::Bool(unsafe { CFBooleanGetValue(v) } != 0));
    }
    cf_to_i64(v).map(AxValue::Number)
}

fn ax_bool(v: CFTypeRef) -> bool {
    matches!(ax_value(v), Some(AxValue::Bool(true)) | Some(AxValue::Number(1)))
}

pub struct AxTeamsReader {
    /// The attribute names as one CFArray (an NSArray, toll-free bridged).
    attrs: Retained<NSArray<NSString>>,
    /// Teams pids already woken with `AXManualAccessibility`.
    hydrated: HashSet<i32>,
    /// Dump the first Teams window even when it isn't a meeting (probe `--any`).
    pub any_window: bool,
    /// Time allowed to copy one window (the probe raises it to capture whole trees).
    pub budget: Duration,
    /// Read this app instead of Teams (probe `--bundle`, for checking the reader on any app).
    pub bundle_id: Option<String>,
}

impl Default for AxTeamsReader {
    fn default() -> Self {
        Self::new()
    }
}

impl AxTeamsReader {
    pub fn new() -> Self {
        let names: Vec<Retained<NSString>> = ATTRS.iter().map(|a| NSString::from_str(a)).collect();
        Self {
            attrs: NSArray::from_retained_slice(&names),
            hydrated: HashSet::new(),
            any_window: false,
            budget: SNAPSHOT_BUDGET * 2,
            bundle_id: None,
        }
    }

    /// Pids of the running app(s) to read. libproc, not `NSWorkspace` (AppKit is main-thread only and
    /// this runs on the meeting watcher's thread several times a second).
    fn app_pids(&self) -> Vec<i32> {
        crate::meetings::mac_procs::running_app_pids(|b| match &self.bundle_id {
            Some(want) => b.eq_ignore_ascii_case(want),
            None => is_teams_bundle(b),
        })
    }

    fn attrs_of(&self, el: CFTypeRef) -> Option<(AxAttrs, CfGuard)> {
        let mut values: CFArrayRef = std::ptr::null();
        let list = Retained::as_ptr(&self.attrs) as CFArrayRef;
        let err = unsafe { AXUIElementCopyMultipleAttributeValues(el, list, 0, &mut values) };
        let values = CfGuard::new(values)?;
        if err != 0 {
            return None;
        }
        let v = array_items(values.as_ptr());
        if v.len() != ATTRS.len() {
            return None;
        }
        let s = |i: usize| cf_to_string(v[i]).unwrap_or_default();
        let attrs = AxAttrs {
            role: s(0),
            title: s(1),
            description: s(2),
            help: s(3),
            value: ax_value(v[4]),
            identifier: s(5),
            dom_identifier: s(6),
            dom_classes: array_items(v[7]).into_iter().filter_map(cf_to_string).collect(),
            selected: ax_bool(v[8]),
        };
        Some((attrs, values))
    }

    /// Copy the subtree under `el` within the depth / node / time limits.
    fn copy(&self, el: CFTypeRef, depth: usize, count: &mut usize, deadline: Instant) -> UiNode {
        *count += 1;
        let Some((attrs, values)) = self.attrs_of(el) else { return UiNode::default() };
        let mut node = to_node(attrs);
        if depth >= MAX_DEPTH {
            return node;
        }
        // Children are borrowed from `values`, which lives until this function returns.
        let children = array_items(array_items(values.as_ptr())[ATTRS.len() - 1]);
        for c in children {
            if *count >= MAX_NODES || Instant::now() > deadline {
                break;
            }
            node.children.push(self.copy(c, depth + 1, count, deadline));
        }
        node
    }

    fn copy_window(&self, win: CFTypeRef) -> UiNode {
        let mut count = 0;
        self.copy(win, 0, &mut count, Instant::now() + self.budget)
    }

    /// Ask Chromium to build its accessibility tree.
    fn hydrate(&mut self, pid: i32, app: &CfGuard) {
        self.hydrated.insert(pid);
        for attr in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
            if let Some(key) = cf_str(attr) {
                unsafe {
                    let _ = AXUIElementSetAttributeValue(app.as_ptr(), key.as_ptr(), kCFBooleanTrue);
                }
            }
        }
        std::thread::sleep(HYDRATE_WAIT);
    }

    /// `(window, title)` for each of the app's windows; the guards keep the elements alive.
    ///
    /// `AXWindows` lists only windows on the current Space, so it comes back empty while Teams sits on
    /// another Space (e.g. Wren or any app is full screen). `AXFocusedWindow` / `AXMainWindow` still
    /// answer then, so fall back to those (focused first: during a call that's usually the meeting).
    fn windows(app: &CfGuard) -> (Vec<CfGuard>, Vec<(CFTypeRef, String)>) {
        let mut keep = Vec::new();
        let mut ptrs: Vec<CFTypeRef> = Vec::new();
        if let Some(arr) = copy_attr(app.as_ptr(), "AXWindows") {
            ptrs = array_items(arr.as_ptr());
            keep.push(arr);
        }
        if ptrs.is_empty() {
            for attr in ["AXFocusedWindow", "AXMainWindow"] {
                if let Some(w) = copy_attr(app.as_ptr(), attr) {
                    if !ptrs.iter().any(|p| unsafe { CFEqual(*p, w.as_ptr()) } != 0) {
                        ptrs.push(w.as_ptr());
                        keep.push(w);
                    }
                }
            }
        }
        let wins = ptrs
            .into_iter()
            .map(|w| {
                let title = copy_attr(w, "AXTitle").and_then(|t| cf_to_string(t.as_ptr())).unwrap_or_default();
                (w, title)
            })
            .collect();
        (keep, wins)
    }
}

impl TeamsReader for AxTeamsReader {
    fn snapshot(&mut self) -> Option<(UiNode, String)> {
        if !crate::hotkey::accessibility_trusted(false) {
            return None;
        }
        for pid in self.app_pids() {
            let Some(app) = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) }) else { continue };
            unsafe { AXUIElementSetMessagingTimeout(app.as_ptr(), AX_TIMEOUT_SECS) };
            let (mut _keep, mut wins) = Self::windows(&app);
            if wins.is_empty() && !self.hydrated.contains(&pid) {
                self.hydrate(pid, &app);
                (_keep, wins) = Self::windows(&app);
            }
            // Meeting windows usually aren't titled just "Microsoft Teams"; try likely ones first.
            wins.sort_by_key(|(_, t)| t.trim().eq_ignore_ascii_case("Microsoft Teams") as u8);
            for (win, title) in wins {
                let mut tree = self.copy_window(win);
                if tree.count() < HYDRATE_BELOW && !self.hydrated.contains(&pid) {
                    self.hydrate(pid, &app);
                    tree = self.copy_window(win);
                }
                if self.any_window || is_meeting_window(&tree) {
                    return Some((tree, title));
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_wins_and_description_moves_to_help() {
        let n = to_node(AxAttrs {
            role: "AXButton".into(),
            title: "Leave".into(),
            description: "Leave the meeting".into(),
            ..Default::default()
        });
        assert_eq!((n.name.as_str(), n.help.as_str()), ("Leave", "Leave the meeting"));
    }

    #[test]
    fn description_is_name_when_no_title() {
        let n = to_node(AxAttrs {
            role: "AXButton".into(),
            description: "Alex Example, Active speaker".into(),
            help: "tip".into(),
            ..Default::default()
        });
        assert_eq!((n.name.as_str(), n.help.as_str()), ("Alex Example, Active speaker", "tip"));
    }

    #[test]
    fn identifier_falls_back_to_dom_id() {
        let n = to_node(AxAttrs { dom_identifier: "hangup-button".into(), ..Default::default() });
        assert_eq!(n.automation_id, "hangup-button");
        let n = to_node(AxAttrs { identifier: "x".into(), dom_identifier: "y".into(), ..Default::default() });
        assert_eq!(n.automation_id, "x");
    }

    #[test]
    fn toggle_values_become_states() {
        let on = to_node(AxAttrs { role: "AXCheckBox".into(), value: Some(AxValue::Number(1)), ..Default::default() });
        assert_eq!(on.states, vec!["toggled_on"]);
        let off = to_node(AxAttrs { role: "AXCheckBox".into(), value: Some(AxValue::Number(0)), selected: true, ..Default::default() });
        assert_eq!(off.states, vec!["toggled_off", "selected"]);
        let slider = to_node(AxAttrs { role: "AXSlider".into(), value: Some(AxValue::Number(40)), ..Default::default() });
        assert!(slider.states.is_empty());
        assert_eq!(slider.value, "40");
    }

    #[test]
    fn classes_join_and_long_values_are_cut() {
        let n = to_node(AxAttrs {
            dom_classes: vec!["a".into(), "b".into()],
            value: Some(AxValue::Text("x".repeat(500))),
            ..Default::default()
        });
        assert_eq!(n.class, "a b");
        assert_eq!(n.value.chars().count(), MAX_VALUE_CHARS);
    }
}
