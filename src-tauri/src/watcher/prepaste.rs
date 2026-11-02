//! Pre-paste editable check (ruling 24): just before pasting, ask UI Automation what has keyboard
//! focus. Only when it is *definitely* not something you can type into (a button, a list, static
//! text…) does Wren hold the paste back and put the text on the clipboard instead. Slow, failed or
//! unsure answers paste as normal — a missed paste costs more than a paste into nothing.
//!
//! The question runs on its own long-lived COM (MTA) thread; the session waits at most
//! `PREPASTE_CHECK_MAX_MS` for the answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded, unbounded};
use windows::Win32::UI::Accessibility::*;

use super::uia::{ComMta, FocusFacts, Uia, control_type_name};
use crate::constants::PREPASTE_CHECK_MAX_MS;

/// What to do with a paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrePaste {
    Paste,
    /// Definitely not editable: copy instead.
    NotEditable,
}

/// The decision from what UI Automation said (`None` = no answer) about the focused element in
/// `process` (the paste target's file name). Pure, so it is table-tested.
///
/// Paste unless the evidence is definite:
/// - terminals are never held back (Windows Terminal's text area is a `Text` control with only a
///   TextPattern — review r1);
/// - a writable value, a text box, a document, a combo box, or a container that says nothing
///   (Pane, Window, Custom, unknown — e.g. Chromium before its accessibility tree is on) → paste;
/// - grids, tables and their cells take Ctrl+V (spreadsheets) → paste;
/// - outside Chromium, any TextPattern makes it unsure → paste. Chromium reports a TextPattern on
///   nearly every node (Claude probe), so there only the control type counts;
/// - held back: controls that never take text (buttons, links, list and tree items, menus, tabs,
///   static text, plain groups…) with no writable value.
pub fn decide(facts: Option<&FocusFacts>, target_pid: u32, process: &str) -> PrePaste {
    let Some(f) = facts else { return PrePaste::Paste };
    if super::TERMINALS.contains(&process.to_lowercase().as_str()) {
        return PrePaste::Paste;
    }
    // Focus reported in another process than the window we paste into: not sure what it means.
    if f.pid == 0 || (target_pid != 0 && f.pid != target_pid) {
        return PrePaste::Paste;
    }
    if f.value_pattern && !f.value_read_only {
        return PrePaste::Paste;
    }
    let chromium = f.framework.eq_ignore_ascii_case("Chrome");
    if f.text_pattern && !chromium {
        return PrePaste::Paste;
    }
    let ct = UIA_CONTROLTYPE_ID(f.control_type);
    let never_text = [
        UIA_ButtonControlTypeId,
        UIA_SplitButtonControlTypeId,
        UIA_CheckBoxControlTypeId,
        UIA_RadioButtonControlTypeId,
        UIA_HyperlinkControlTypeId,
        UIA_ImageControlTypeId,
        UIA_ListControlTypeId,
        UIA_ListItemControlTypeId,
        UIA_TreeControlTypeId,
        UIA_TreeItemControlTypeId,
        UIA_MenuControlTypeId,
        UIA_MenuBarControlTypeId,
        UIA_MenuItemControlTypeId,
        UIA_TabControlTypeId,
        UIA_TabItemControlTypeId,
        UIA_ToolBarControlTypeId,
        UIA_StatusBarControlTypeId,
        UIA_ProgressBarControlTypeId,
        UIA_SliderControlTypeId,
        UIA_ScrollBarControlTypeId,
        UIA_HeaderControlTypeId,
        UIA_HeaderItemControlTypeId,
        UIA_TitleBarControlTypeId,
        UIA_ThumbControlTypeId,
        UIA_SeparatorControlTypeId,
        UIA_ToolTipControlTypeId,
        UIA_TextControlTypeId,
        UIA_GroupControlTypeId,
    ];
    if never_text.contains(&ct) { PrePaste::NotEditable } else { PrePaste::Paste }
}

struct Request {
    target_pid: u32,
    reply: Sender<Option<FocusFacts>>,
}

/// The long-lived checker. Cheap to clone.
#[derive(Clone)]
pub struct Checker {
    tx: Sender<Request>,
    busy: Arc<AtomicBool>,
}

impl Checker {
    pub fn start() -> anyhow::Result<Self> {
        let (tx, rx) = unbounded::<Request>();
        let busy = Arc::new(AtomicBool::new(false));
        let worker_busy = busy.clone();
        std::thread::Builder::new().name("wren-prepaste".into()).spawn(move || {
            let _com = ComMta::join();
            let mut uia: Option<Uia> = None;
            for req in rx {
                if uia.is_none() {
                    uia = Uia::new(PREPASTE_CHECK_MAX_MS).map_err(|e| tracing::warn!(error = %e, "pre-paste check: no UI Automation")).ok();
                }
                let facts = uia.as_ref().and_then(|u| u.focused().ok()).map(|f| f.facts);
                let _ = (req.target_pid, req.reply.send(facts));
                worker_busy.store(false, Ordering::SeqCst);
            }
        })?;
        Ok(Self { tx, busy })
    }

    /// Asks the question once without waiting for the answer, when a recording starts. Chromium
    /// (Edge, Chrome, Electron apps such as Claude) switches its accessibility tree on only when a
    /// client first asks, and until then reports the focused element as a bare `Pane`; asking early
    /// means the real check at paste time sees the actual element.
    pub fn prime(&self) {
        if self.busy.swap(true, Ordering::SeqCst) {
            return;
        }
        let (reply, _) = bounded(1);
        if self.tx.send(Request { target_pid: 0, reply }).is_err() {
            self.busy.store(false, Ordering::SeqCst);
        }
    }

    /// Asks, waits at most `PREPASTE_CHECK_MAX_MS`, decides. Logs the decision (no text).
    pub fn check(&self, target_pid: u32, app: &str) -> PrePaste {
        let t = Instant::now();
        if self.busy.swap(true, Ordering::SeqCst) {
            tracing::info!(%app, "pre-paste check still busy with the last one; pasting");
            return PrePaste::Paste;
        }
        let (reply, rx) = bounded(1);
        if self.tx.send(Request { target_pid, reply }).is_err() {
            self.busy.store(false, Ordering::SeqCst);
            return PrePaste::Paste;
        }
        let facts = match rx.recv_timeout(Duration::from_millis(PREPASTE_CHECK_MAX_MS)) {
            Ok(f) => f,
            Err(_) => {
                // The worker clears `busy` itself when the slow answer finally arrives.
                tracing::info!(%app, ms = t.elapsed().as_millis() as u64, "pre-paste check too slow; pasting");
                return PrePaste::Paste;
            }
        };
        let decision = decide(facts.as_ref(), target_pid, app);
        tracing::info!(
            %app,
            ms = t.elapsed().as_millis() as u64,
            control = facts.as_ref().map(|f| control_type_name(f.control_type)).unwrap_or("none"),
            writable_value = facts.as_ref().is_some_and(|f| f.value_pattern && !f.value_read_only),
            decision = ?decision,
            "pre-paste check"
        );
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Facts as Chromium reports them (text and TextEdit patterns on nearly every node).
    fn chrome(ct: UIA_CONTROLTYPE_ID, value: Option<bool>) -> FocusFacts {
        FocusFacts {
            control_type: ct.0,
            pid: 10,
            value_pattern: value.is_some(),
            value_read_only: value.unwrap_or(true),
            text_pattern: true,
            text_edit_pattern: true,
            keyboard_focusable: true,
            framework: "Chrome".into(),
            ..Default::default()
        }
    }

    /// Facts from a native (Win32 / XAML / WPF) app.
    fn native(ct: UIA_CONTROLTYPE_ID, framework: &str, value: Option<bool>, text: bool) -> FocusFacts {
        FocusFacts {
            control_type: ct.0,
            pid: 10,
            value_pattern: value.is_some(),
            value_read_only: value.unwrap_or(true),
            text_pattern: text,
            keyboard_focusable: true,
            framework: framework.into(),
            ..Default::default()
        }
    }

    #[test]
    fn only_definite_non_text_targets_are_held_back() {
        use PrePaste::*;
        let wt = FocusFacts { class_name: "TermControl".into(), ..native(UIA_TextControlTypeId, "XAML", None, true) };
        let table: &[(FocusFacts, &str, PrePaste, &str)] = &[
            // Text boxes, documents and anything unsure paste.
            (chrome(UIA_EditControlTypeId, Some(false)), "msedge.exe", Paste, "text box"),
            (chrome(UIA_EditControlTypeId, None), "msedge.exe", Paste, "edit without value"),
            (chrome(UIA_EditControlTypeId, Some(true)), "msedge.exe", Paste, "read-only edit: unsure"),
            (chrome(UIA_DocumentControlTypeId, Some(true)), "msedge.exe", Paste, "page document: unsure"),
            (chrome(UIA_PaneControlTypeId, None), "claude.exe", Paste, "Chromium before accessibility is on"),
            (native(UIA_WindowControlTypeId, "Win32", None, false), "game.exe", Paste, "window"),
            (native(UIA_CustomControlTypeId, "", None, false), "app.exe", Paste, "custom"),
            (chrome(UIA_GroupControlTypeId, Some(false)), "msedge.exe", Paste, "editable group (writable value)"),
            // Terminals (review r1, Critical): Windows Terminal's text area is Text + TextPattern, no value.
            (wt.clone(), "windowsterminal.exe", Paste, "Windows Terminal"),
            (wt, "someotherhost.exe", Paste, "TermControl hosted elsewhere: a text pattern outside Chromium"),
            (native(UIA_DocumentControlTypeId, "Win32", None, true), "conhost.exe", Paste, "conhost"),
            (native(UIA_ButtonControlTypeId, "Win32", None, false), "powershell.exe", Paste, "a terminal process is never held back"),
            // Spreadsheet-like cells take Ctrl+V (review r1, Important).
            (native(UIA_DataItemControlTypeId, "Win32", None, false), "excel.exe", Paste, "Excel cell"),
            (native(UIA_DataGridControlTypeId, "WPF", None, false), "app.exe", Paste, "data grid"),
            (native(UIA_TableControlTypeId, "WPF", None, false), "app.exe", Paste, "table"),
            (chrome(UIA_DataItemControlTypeId, None), "msedge.exe", Paste, "grid cell in a web app"),
            // A native control with a text pattern is unsure.
            (native(UIA_TextControlTypeId, "WPF", None, true), "app.exe", Paste, "native text with a text pattern"),
            // Held back: what the evidence supports.
            (chrome(UIA_GroupControlTypeId, Some(true)), "msedge.exe", NotEditable, "Chromium plain group (test page ?focus=none)"),
            (chrome(UIA_ButtonControlTypeId, None), "msedge.exe", NotEditable, "Chromium button"),
            (chrome(UIA_HyperlinkControlTypeId, Some(true)), "msedge.exe", NotEditable, "Chromium link"),
            (chrome(UIA_ListItemControlTypeId, None), "claude.exe", NotEditable, "Chromium list item"),
            (native(UIA_ListItemControlTypeId, "Win32", None, false), "explorer.exe", NotEditable, "desktop icon"),
            (native(UIA_ButtonControlTypeId, "Win32", None, false), "app.exe", NotEditable, "native button"),
            (native(UIA_TextControlTypeId, "Win32", None, false), "app.exe", NotEditable, "static label, no text pattern"),
        ];
        for (f, process, want, what) in table {
            assert_eq!(decide(Some(f), 10, process), *want, "{what}");
        }
    }

    #[test]
    fn no_answer_or_another_process_pastes() {
        assert_eq!(decide(None, 10, "app.exe"), PrePaste::Paste);
        let button = chrome(UIA_ButtonControlTypeId, None);
        assert_eq!(decide(Some(&button), 11, "msedge.exe"), PrePaste::Paste, "focus reported elsewhere");
        assert_eq!(decide(Some(&FocusFacts { pid: 0, ..button.clone() }), 10, "msedge.exe"), PrePaste::Paste);
        assert_eq!(decide(Some(&FocusFacts { control_type: 0, ..button }), 10, "msedge.exe"), PrePaste::Paste, "unknown type");
    }
}
