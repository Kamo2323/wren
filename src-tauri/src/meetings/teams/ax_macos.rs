//! macOS reader for the Teams meeting window (plan Task 6), the counterpart of `uia.rs`: finds the
//! running Teams app(s) and copies the meeting window's Accessibility (AX) tree into a [`UiNode`].
//! Read-only except for setting `AXManualAccessibility` / `AXEnhancedUserInterface` on the Teams app
//! element until the call window's tree can be read (Chromium/Electron only build the tree when asked);
//! both are cleared again on the processes Wren set them on when the last reader is dropped (the
//! meeting's watcher stopped), so Teams doesn't keep paying for its accessibility tree after the
//! meeting. Readers are counted process-wide ([`Registry`]): an old meeting's reader that goes late
//! never switches accessibility off under a newer meeting's reader.
//!
//! Needs the Accessibility permission (the same one dictation uses). Names and titles read here are
//! never logged.

use super::snapshot::UiNode;
use super::{is_meeting_window, TeamsReader, MAX_DEPTH, MAX_NODES, SNAPSHOT_BUDGET};
use crate::meetings::capture::mac_tap::is_teams_bundle;
use crate::watcher::ax_macos::{cf_str, cf_to_i64, cf_to_string, copy_attr, CfGuard};
use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSString};
use std::collections::HashMap;
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
    static kCFBooleanFalse: CFTypeRef;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, idx: isize) -> CFTypeRef;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
}

/// Cross-process AX calls give up after this long (Teams can hang while busy). The timeout belongs
/// to each element ref (the app's doesn't carry over to windows or children copied from it), so
/// [`limit`] is applied to every element this reader messages. The system-wide element's timeout is
/// process-global and owned by the dictation watcher, so it is left alone.
const AX_TIMEOUT_SECS: f32 = 0.4;

/// Cap how long AX calls on `el` may block (a stalled Teams renderer would otherwise hold the
/// watcher for the 6 s default). Local to this process: no IPC.
fn limit(el: CFTypeRef) {
    if !el.is_null() {
        unsafe { AXUIElementSetMessagingTimeout(el, AX_TIMEOUT_SECS) };
    }
}
/// How long Chromium takes to build its tree after being woken.
const HYDRATE_WAIT: Duration = Duration::from_millis(600);
/// The attributes that make Chromium/Electron build their accessibility tree.
const WAKE_ATTRS: [&str; 2] = ["AXManualAccessibility", "AXEnhancedUserInterface"];
/// Reads in a row without a readable call window, after it had been read, before waking again (one
/// slow or timed-out read is not a lost tree; at the 250 ms poll this is about a second).
const REWAKE_AFTER_MISSES: u32 = 4;

/// Waking one Teams process: retried with backoff until the call window's tree (the one with
/// `hangup-button`) can be read. Teams' chat/main window having a tree says nothing about the separate
/// call window, so only the call window counts. When it can't be read any more (it closed, or its
/// tree went empty) the wake starts over, with the same backoff.
#[derive(Debug, Clone, Default, PartialEq)]
struct Wake {
    attempts: u32,
    next: Option<Instant>,
    /// The call window's tree was read on the latest reads.
    done: bool,
    /// Reads in a row without a readable call window since `done`.
    misses: u32,
}

impl Wake {
    fn due(&self, now: Instant) -> bool {
        !self.done && self.next.is_none_or(|n| now >= n)
    }

    fn tried(&mut self, now: Instant) {
        self.attempts += 1;
        self.next = Some(now + wake_backoff(self.attempts));
    }

    /// One read of this process's windows: whether a call window's tree was readable in it.
    fn read(&mut self, call_window: bool) {
        if call_window {
            self.done = true;
            self.misses = 0;
        } else if self.done {
            self.misses += 1;
            if self.misses >= REWAKE_AFTER_MISSES {
                // Wake again: due as soon as the backoff since the last attempt has passed.
                self.done = false;
                self.misses = 0;
            }
        }
    }
}

/// Wait after the `attempts`-th wake that didn't yield a readable call window: 2 s, 4 s, 8 s … at most 60 s.
fn wake_backoff(attempts: u32) -> Duration {
    Duration::from_secs((2u64 << attempts.saturating_sub(1).min(5)).min(60))
}

/// Process-wide: the live readers, and the Teams processes Wren switched accessibility on for (pid →
/// executable path then, so it is cleared on the same process only). Only the last reader to go
/// clears them; a reader created meanwhile keeps them (and re-applies them itself if they are lost).
#[derive(Debug, Default)]
struct Registry {
    live: usize,
    woken: HashMap<i32, String>,
}

impl Registry {
    fn join(&mut self) {
        self.live += 1;
    }

    fn woke(&mut self, pid: i32, exe: Option<String>) {
        if let Some(exe) = exe {
            self.woken.entry(pid).or_insert(exe);
        }
    }

    /// A reader went; the processes to clear (only when it was the last one).
    fn leave(&mut self) -> Vec<(i32, String)> {
        self.live = self.live.saturating_sub(1);
        if self.live > 0 {
            return Vec::new();
        }
        self.woken.drain().collect()
    }
}

/// Held while a reader joins, wakes Teams or clears the attributes, so a late clear and a new
/// reader's wake can't interleave.
static REGISTRY: std::sync::Mutex<Option<Registry>> = std::sync::Mutex::new(None);

fn registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut g = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Registry::default))
}
/// Look for Teams processes again at most this often while the ones found still have windows (a
/// full process scan is a few hundred libproc calls; the watcher polls several times a second).
const RESCAN: Duration = Duration::from_secs(5);
/// ... and at most this often while the cached pids have no windows (Teams quit or restarted).
const RESCAN_MISSED: Duration = Duration::from_secs(1);

/// Whether the cached pids (scanned at `at`) are due a rescan at `now`; `missed`: they had no window.
fn rescan_due(at: Option<Instant>, missed: bool, now: Instant) -> bool {
    at.is_none_or(|at| {
        let age = now.saturating_duration_since(at);
        age >= RESCAN || (missed && age >= RESCAN_MISSED)
    })
}

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
    /// Per Teams pid: waking its accessibility tree (attempts, backoff, whether the call window reads).
    wake: HashMap<i32, Wake>,
    /// The Teams pids last found, when, and whether none of them had a window since.
    pids: Vec<i32>,
    scanned: Option<Instant>,
    missed: bool,
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
        // Waits for a previous meeting's reader that is clearing the attributes right now.
        registry(Registry::join);
        Self {
            attrs: NSArray::from_retained_slice(&names),
            wake: HashMap::new(),
            pids: Vec::new(),
            scanned: None,
            missed: false,
            any_window: false,
            budget: SNAPSHOT_BUDGET * 2,
            bundle_id: None,
        }
    }

    /// Pids of the running app(s) to read: cached, rescanned per [`rescan_due`]. libproc, not
    /// `NSWorkspace` (AppKit is main-thread only and this runs on the meeting watcher's thread).
    fn app_pids(&mut self) -> Vec<i32> {
        let now = Instant::now();
        if rescan_due(self.scanned, self.missed, now) {
            self.pids = crate::meetings::mac_procs::running_app_pids(|b| match &self.bundle_id {
                Some(want) => b.eq_ignore_ascii_case(want),
                None => is_teams_bundle(b),
            });
            self.scanned = Some(now);
            self.missed = false;
        }
        self.pids.clone()
    }

    fn attrs_of(&self, el: CFTypeRef) -> Option<(AxAttrs, CfGuard)> {
        limit(el);
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

    /// Whether `pid` should be woken now (call window not readable, backoff passed).
    fn wake_due(&self, pid: i32) -> bool {
        self.wake.get(&pid).is_none_or(|w| w.due(Instant::now()))
    }

    /// Ask Chromium to build its accessibility tree. Not "done" until the call window reads: retried
    /// with backoff ([`wake_backoff`]) while it doesn't.
    fn hydrate(&mut self, pid: i32, app: &CfGuard) {
        registry(|r| {
            if !r.woken.contains_key(&pid) {
                r.woke(pid, crate::meetings::mac_procs::pid_path(pid));
            }
            set_wake_attrs(app, true);
        });
        std::thread::sleep(HYDRATE_WAIT);
        self.wake.entry(pid).or_default().tried(Instant::now());
    }

    /// The first meeting window among `wins` (or, for the probe's `any_window`, the first window).
    fn find_meeting(&self, wins: Vec<(CFTypeRef, String)>) -> Option<(UiNode, String)> {
        for (win, title) in wins {
            let tree = self.copy_window(win);
            if self.any_window || is_meeting_window(&tree) {
                return Some((tree, title));
            }
        }
        None
    }

    /// `(window, title)` for each of the app's windows; the guards keep the elements alive.
    ///
    /// `AXWindows` lists only windows on the current Space, so it misses the call window while that
    /// sits on another Space or is full screen, even when other Teams windows are listed.
    /// `AXFocusedWindow` / `AXMainWindow` still answer then, so they are always added (first: during a
    /// call the focused one is usually the meeting), without duplicates.
    fn windows(app: &CfGuard) -> (Vec<CfGuard>, Vec<(CFTypeRef, String)>) {
        let mut keep = Vec::new();
        let mut ptrs: Vec<CFTypeRef> = Vec::new();
        let add = |ptrs: &mut Vec<CFTypeRef>, w: CFTypeRef| {
            if !ptrs.iter().any(|p| unsafe { CFEqual(*p, w) } != 0) {
                limit(w);
                ptrs.push(w);
            }
        };
        for attr in ["AXFocusedWindow", "AXMainWindow"] {
            if let Some(w) = copy_attr(app.as_ptr(), attr) {
                add(&mut ptrs, w.as_ptr());
                keep.push(w);
            }
        }
        if let Some(arr) = copy_attr(app.as_ptr(), "AXWindows") {
            for w in array_items(arr.as_ptr()) {
                add(&mut ptrs, w);
            }
            keep.push(arr);
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

/// Set (or clear) the attributes that make Chromium/Electron build their accessibility tree.
fn set_wake_attrs(app: &CfGuard, on: bool) {
    for attr in WAKE_ATTRS {
        if let Some(key) = cf_str(attr) {
            unsafe {
                let _ = AXUIElementSetAttributeValue(app.as_ptr(), key.as_ptr(), if on { kCFBooleanTrue } else { kCFBooleanFalse });
            }
        }
    }
}

impl Drop for AxTeamsReader {
    /// The meeting's watcher stopped. If this was the last reader, clear the wake attributes on the
    /// Teams processes Wren set them on (only while the same executable still runs under that pid), so
    /// Teams stops maintaining its accessibility tree for us. A newer meeting's reader still running
    /// keeps them on. The registry stays locked while clearing, so a reader starting meanwhile wakes
    /// Teams only after the clear.
    fn drop(&mut self) {
        registry(|r| {
            for (pid, exe) in r.leave() {
                if crate::meetings::mac_procs::pid_path(pid).as_deref() != Some(exe.as_str()) {
                    continue;
                }
                if let Some(app) = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) }) {
                    limit(app.as_ptr());
                    set_wake_attrs(&app, false);
                }
            }
        });
    }
}

impl TeamsReader for AxTeamsReader {
    fn snapshot(&mut self) -> Option<(UiNode, String)> {
        if !crate::hotkey::accessibility_trusted(false) {
            return None;
        }
        let pids = self.app_pids();
        let mut any_window = false;
        for pid in pids.iter().copied() {
            let Some(app) = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) }) else { continue };
            limit(app.as_ptr());
            // Meeting windows usually aren't titled just "Microsoft Teams"; try likely ones first.
            let windows = |app: &CfGuard| {
                let (keep, mut wins) = Self::windows(app);
                wins.sort_by_key(|(_, t)| t.trim().eq_ignore_ascii_case("Microsoft Teams") as u8);
                (keep, wins)
            };
            let (mut _keep, wins) = windows(&app);
            any_window |= !wins.is_empty();
            let mut found = self.find_meeting(wins);
            // No readable call window (none open, or its tree isn't built: the chat window having a
            // tree doesn't mean the call window has one): wake, then read every window again.
            if found.is_none() && self.wake_due(pid) {
                self.hydrate(pid, &app);
                let wins;
                (_keep, wins) = windows(&app);
                any_window |= !wins.is_empty();
                found = self.find_meeting(wins);
            }
            let call_window = found.as_ref().is_some_and(|(tree, _)| is_meeting_window(tree));
            self.wake.entry(pid).or_default().read(call_window);
            if found.is_some() {
                return found;
            }
        }
        // Cached pids without windows: Teams may have quit or restarted; look again soon.
        self.missed = !pids.is_empty() && !any_window;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_retries_with_backoff_until_a_usable_tree() {
        let t0 = Instant::now();
        let mut w = Wake::default();
        assert!(w.due(t0), "first wake is immediate");
        w.tried(t0);
        assert!(!w.due(t0 + Duration::from_secs(1)), "not again right away");
        assert!(w.due(t0 + Duration::from_secs(2)), "but retried: an empty tree isn't 'done'");
        w.tried(t0);
        assert!(!w.due(t0 + Duration::from_secs(3)));
        assert!(w.due(t0 + Duration::from_secs(4)));
        w.read(true);
        assert!(!w.due(t0 + Duration::from_secs(3_600)), "done once the call window was read");
        assert_eq!(wake_backoff(1), Duration::from_secs(2));
        assert_eq!(wake_backoff(3), Duration::from_secs(8));
        assert_eq!(wake_backoff(6), Duration::from_secs(60));
        assert_eq!(wake_backoff(40), Duration::from_secs(60));
    }

    #[test]
    fn only_the_call_window_ends_the_wake_and_losing_it_wakes_again() {
        let t0 = Instant::now();
        let mut w = Wake::default();
        // The chat/main window has a big tree, but no call window was read: keep waking.
        w.tried(t0);
        w.read(false);
        assert!(w.due(t0 + wake_backoff(1)), "a big chat window is not 'done'");
        // The call window (hangup-button) read: done.
        w.read(true);
        assert!(!w.due(t0 + Duration::from_secs(600)));
        // One slow read without it is not a lost tree.
        for _ in 0..REWAKE_AFTER_MISSES - 1 {
            w.read(false);
        }
        assert!(!w.due(t0 + Duration::from_secs(600)));
        w.read(true);
        assert_eq!(w.misses, 0, "a read in between starts the count over");
        // The call window closed or its tree went empty: wake again, on the existing backoff.
        for _ in 0..REWAKE_AFTER_MISSES {
            w.read(false);
        }
        assert!(!w.done);
        assert!(w.due(t0 + wake_backoff(1)), "due once the backoff since the last wake has passed");
        assert!(!w.due(t0), "not before it");
        w.tried(t0 + wake_backoff(1));
        assert!(!w.due(t0 + wake_backoff(1) + Duration::from_secs(1)), "and backs off again");
    }

    #[test]
    fn only_the_last_reader_clears_and_a_new_one_keeps_the_wake() {
        let mut r = Registry::default();
        // Meeting A's reader wakes Teams.
        r.join();
        r.woke(500, Some("/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams".into()));
        // Meeting B starts before A's reader has gone.
        r.join();
        r.woke(500, Some("/other".into()));
        // A's late drop must not switch accessibility off under B.
        assert!(r.leave().is_empty());
        assert_eq!(r.live, 1);
        // B goes last: clear, on the process first woken.
        assert_eq!(r.leave(), vec![(500, "/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams".to_string())]);
        assert!(r.woken.is_empty());
        // A reader that never woke anything clears nothing; an unknown executable isn't recorded.
        r.join();
        r.woke(501, None);
        assert!(r.leave().is_empty());
        // Never underflows.
        assert!(r.leave().is_empty());
        assert_eq!(r.live, 0);
        // Old reader gone first, then a new one wakes: it is the one to clear later.
        r.join();
        r.woke(600, Some("/t".into()));
        assert_eq!(r.leave(), vec![(600, "/t".to_string())]);
    }

    #[test]
    #[ignore = "live: needs Teams running and Accessibility permission; toggles Teams' AX wake attributes"]
    fn live_wake_attributes_are_cleared_when_the_reader_drops() {
        if !crate::hotkey::accessibility_trusted(false) {
            eprintln!("not trusted for Accessibility; skipped");
            return;
        }
        let mut reader = AxTeamsReader::new();
        reader.any_window = true;
        let Some(&pid) = reader.app_pids().first() else {
            eprintln!("Teams isn't running; skipped");
            return;
        };
        let read = || {
            let app = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap();
            limit(app.as_ptr());
            WAKE_ATTRS.map(|a| copy_attr(app.as_ptr(), a).map(|v| ax_bool(v.as_ptr())))
        };
        let before = read();
        let app = CfGuard::new(unsafe { AXUIElementCreateApplication(pid) }).unwrap();
        limit(app.as_ptr());
        reader.hydrate(pid, &app);
        let woken = read();
        let snap = reader.snapshot();
        let call = snap.as_ref().is_some_and(|(t, _)| is_meeting_window(t));
        let snap = snap.map(|(t, _)| t.count());
        let done = reader.wake.get(&pid).map(|w| w.done);
        drop(reader);
        let after = read();
        eprintln!("Teams AX wake attrs {WAKE_ATTRS:?}: before {before:?}, woken {woken:?}, after drop {after:?}; window nodes {snap:?}, done {done:?}");
        // Chromium accepts AXManualAccessibility but doesn't report it back; AXEnhancedUserInterface reads.
        assert!(woken.iter().all(|v| *v != Some(false)) && woken[1] == Some(true));
        assert!(after.iter().all(|v| *v != Some(true)), "cleared when the watcher's reader drops");
        assert_eq!(done, Some(call), "done exactly when the call window was read");
    }

    #[test]
    #[ignore = "live: needs Teams and Accessibility permission; join a Teams call while it runs (WREN_AX_SECS, default 30)"]
    fn live_reader_wakes_the_call_window() {
        if !crate::hotkey::accessibility_trusted(false) {
            eprintln!("not trusted for Accessibility; skipped");
            return;
        }
        let secs: u64 = std::env::var("WREN_AX_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(30);
        let mut reader = AxTeamsReader::new();
        let t0 = Instant::now();
        let mut last = String::new();
        while t0.elapsed() < Duration::from_secs(secs) {
            let snap = reader.snapshot();
            let wake: Vec<(u32, bool)> = reader.wake.values().map(|w| (w.attempts, w.done)).collect();
            // Counts and booleans only: names are never printed.
            let line = match &snap {
                Some((tree, title)) => {
                    let o = super::super::parse(tree, title);
                    format!(
                        "call window: own tile {}, self muted {:?}, {} speaking, {} in roster, title {}; wake (attempts, done) {wake:?}",
                        o.self_name.is_some(),
                        o.self_muted,
                        o.speaking.len(),
                        o.roster.len(),
                        o.title.is_some()
                    )
                }
                None => format!("no call window; wake (attempts, done) {wake:?}"),
            };
            if line != last {
                eprintln!("{:>5.1}s {line}", t0.elapsed().as_secs_f32());
                last = line;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    #[test]
    fn teams_pids_are_rescanned_every_5s_or_soon_after_a_miss() {
        let t0 = Instant::now();
        assert!(rescan_due(None, false, t0), "first read scans");
        assert!(!rescan_due(Some(t0), false, t0 + Duration::from_millis(250)), "not on every poll");
        assert!(!rescan_due(Some(t0), false, t0 + Duration::from_millis(4_900)));
        assert!(rescan_due(Some(t0), false, t0 + RESCAN));
        // The cached pids had no window (Teams restarted under a new pid): sooner, but not every poll.
        assert!(!rescan_due(Some(t0), true, t0 + Duration::from_millis(500)));
        assert!(rescan_due(Some(t0), true, t0 + RESCAN_MISSED));
    }

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
