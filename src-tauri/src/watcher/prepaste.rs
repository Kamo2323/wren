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

/// The decision from what UI Automation said (`None` = no answer). Pure, so it is table-tested.
pub fn decide(facts: Option<&FocusFacts>, target_pid: u32) -> PrePaste {
    let Some(f) = facts else { return PrePaste::Paste };
    // Focus reported in another process than the window we paste into: not sure what it means.
    if f.pid == 0 || (target_pid != 0 && f.pid != target_pid) {
        return PrePaste::Paste;
    }
    if f.value_pattern && !f.value_read_only {
        return PrePaste::Paste;
    }
    let ct = UIA_CONTROLTYPE_ID(f.control_type);
    // Text boxes and documents (including read-only ones: Chromium can report a page document
    // before its accessibility tree is complete), and containers that say nothing (an app with no
    // accessibility, Electron before it switches accessibility on, games, custom controls).
    let unsure = [
        UIA_EditControlTypeId,
        UIA_DocumentControlTypeId,
        UIA_ComboBoxControlTypeId,
        UIA_PaneControlTypeId,
        UIA_WindowControlTypeId,
        UIA_CustomControlTypeId,
    ];
    if f.control_type == 0 || unsure.contains(&ct) {
        return PrePaste::Paste;
    }
    let not_text = [
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
        UIA_TableControlTypeId,
        UIA_DataGridControlTypeId,
        UIA_DataItemControlTypeId,
        UIA_TextControlTypeId,
        UIA_GroupControlTypeId,
        UIA_TitleBarControlTypeId,
        UIA_ThumbControlTypeId,
        UIA_SeparatorControlTypeId,
        UIA_ToolTipControlTypeId,
        UIA_CalendarControlTypeId,
        UIA_SpinnerControlTypeId,
        UIA_SemanticZoomControlTypeId,
        UIA_AppBarControlTypeId,
    ];
    if not_text.contains(&ct) { PrePaste::NotEditable } else { PrePaste::Paste }
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
        let decision = decide(facts.as_ref(), target_pid);
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

    fn facts(ct: UIA_CONTROLTYPE_ID, value: Option<bool>) -> FocusFacts {
        FocusFacts {
            control_type: ct.0,
            pid: 10,
            value_pattern: value.is_some(),
            value_read_only: value.unwrap_or(true),
            text_pattern: true,
            text_edit_pattern: true, // Chromium reports it on nearly everything
            keyboard_focusable: true,
            ..Default::default()
        }
    }

    #[test]
    fn only_definite_non_text_targets_are_held_back() {
        use PrePaste::*;
        let table: &[(FocusFacts, PrePaste, &str)] = &[
            (facts(UIA_EditControlTypeId, Some(false)), Paste, "text box"),
            (facts(UIA_EditControlTypeId, None), Paste, "edit without value"),
            (facts(UIA_EditControlTypeId, Some(true)), Paste, "read-only edit: unsure"),
            (facts(UIA_DocumentControlTypeId, Some(true)), Paste, "page document: unsure"),
            (facts(UIA_PaneControlTypeId, None), Paste, "a pane says nothing"),
            (facts(UIA_WindowControlTypeId, None), Paste, "window"),
            (facts(UIA_CustomControlTypeId, None), Paste, "custom"),
            (facts(UIA_GroupControlTypeId, Some(false)), Paste, "editable group (writable value)"),
            (facts(UIA_GroupControlTypeId, Some(true)), NotEditable, "plain group"),
            (facts(UIA_TextControlTypeId, None), NotEditable, "static text"),
            (facts(UIA_ButtonControlTypeId, None), NotEditable, "button"),
            (facts(UIA_ListItemControlTypeId, None), NotEditable, "list item (desktop icons)"),
            (facts(UIA_HyperlinkControlTypeId, Some(true)), NotEditable, "link"),
        ];
        for (f, want, what) in table {
            assert_eq!(decide(Some(f), 10), *want, "{what}");
        }
    }

    #[test]
    fn no_answer_or_another_process_pastes() {
        assert_eq!(decide(None, 10), PrePaste::Paste);
        let button = facts(UIA_ButtonControlTypeId, None);
        assert_eq!(decide(Some(&button), 11), PrePaste::Paste, "focus reported elsewhere");
        assert_eq!(decide(Some(&FocusFacts { pid: 0, ..button.clone() }), 10), PrePaste::Paste);
        assert_eq!(decide(Some(&FocusFacts { control_type: 0, ..button }), 10), PrePaste::Paste, "unknown type");
    }
}
