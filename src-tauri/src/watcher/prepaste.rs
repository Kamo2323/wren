//! Pre-paste editable check (ruling 24): just before pasting, ask OS Accessibility (Windows UI
//! Automation or macOS `AXUIElement`) what has keyboard focus. Only when it is *definitely* not
//! something you can type into (a button, a list, static text…) does Wren hold the paste back and
//! put the text on the clipboard instead. Slow, failed or unsure answers paste as normal — a missed
//! paste costs more than a paste into nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded, unbounded};

use crate::constants::PREPASTE_CHECK_MAX_MS;

/// What to do with a paste.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrePaste {
    Paste,
    /// Definitely not editable: copy instead.
    NotEditable,
}

// ============================================================================================
// Windows implementation (UI Automation)
// ============================================================================================
#[cfg(windows)]
mod platform {
    use super::*;
    use crate::watcher::uia::{ComMta, FocusFacts, Uia, control_type_name};
    use windows::Win32::UI::Accessibility::*;

    pub fn decide(facts: Option<&FocusFacts>, target_pid: u32, process: &str) -> PrePaste {
        let Some(f) = facts else { return PrePaste::Paste };
        if crate::watcher::TERMINALS.contains(&process.to_lowercase().as_str()) {
            return PrePaste::Paste;
        }
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
                        uia = Uia::new(PREPASTE_CHECK_MAX_MS)
                            .map_err(|e| tracing::warn!(error = %e, "pre-paste check: no UI Automation"))
                            .ok();
                    }
                    let facts = uia.as_ref().and_then(|u| u.focused().ok()).map(|f| f.facts);
                    let _ = (req.target_pid, req.reply.send(facts));
                    worker_busy.store(false, Ordering::SeqCst);
                }
            })?;
            Ok(Self { tx, busy })
        }

        pub fn prime(&self) {
            if self.busy.swap(true, Ordering::SeqCst) {
                return;
            }
            let (reply, _) = bounded(1);
            if self.tx.send(Request { target_pid: 0, reply }).is_err() {
                self.busy.store(false, Ordering::SeqCst);
            }
        }

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
}

// ============================================================================================
// macOS implementation (AXUIElement)
// ============================================================================================
#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use crate::watcher::ax_macos::{self, AxClient, FocusFacts};

    /// Pure decision on macOS Accessibility facts (`None` = no answer -> paste).
    pub fn decide(facts: Option<&FocusFacts>, target_pid: u32, process: &str) -> PrePaste {
        let Some(f) = facts else { return PrePaste::Paste };
        if crate::watcher::TERMINALS.contains(&process.to_lowercase().as_str()) {
            return PrePaste::Paste;
        }
        if f.pid == 0 || (target_pid != 0 && f.pid != target_pid) {
            return PrePaste::Paste;
        }
        if f.value_settable {
            return PrePaste::Paste;
        }
        // Standard editable roles always paste; containers (AXWindow, AXWebArea, AXGroup, AXScrollArea,
        // AXUnknown, AXCell, AXTable, AXRow) are unsure -> paste.
        let never_text = [
            "AXButton",
            "AXCheckBox",
            "AXRadioButton",
            "AXPopUpButton",
            "AXMenuButton",
            "AXMenu",
            "AXMenuBar",
            "AXMenuBarItem",
            "AXMenuItem",
            "AXLink",
            "AXImage",
            "AXStaticText",
            "AXSlider",
            "AXScrollBar",
            "AXToolbar",
            "AXTabGroup",
            "AXRadioGroup",
            "AXList",
            "AXOutline",
            "AXBrowser",
            "AXProgressIndicator",
            "AXBusyIndicator",
            "AXSplitter",
        ];
        if never_text.contains(&f.role.as_str()) {
            PrePaste::NotEditable
        } else {
            PrePaste::Paste
        }
    }

    enum Request {
        Prime,
        Check { reply: Sender<Option<FocusFacts>> },
    }

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
                let mut client: Option<AxClient> = None;
                for req in rx {
                    match req {
                        Request::Prime => {
                            ax_macos::prime_frontmost();
                        }
                        Request::Check { reply } => {
                            let facts = if crate::hotkey::accessibility_trusted(false) {
                                if client.is_none() {
                                    client = AxClient::new(PREPASTE_CHECK_MAX_MS).ok();
                                }
                                client.as_ref().and_then(|c| c.focused().ok()).map(|f| f.facts)
                            } else {
                                None
                            };
                            let _ = reply.send(facts);
                        }
                    }
                    worker_busy.store(false, Ordering::SeqCst);
                }
            })?;
            Ok(Self { tx, busy })
        }

        pub fn prime(&self) {
            if self.busy.swap(true, Ordering::SeqCst) {
                return;
            }
            if self.tx.send(Request::Prime).is_err() {
                self.busy.store(false, Ordering::SeqCst);
            }
        }

        pub fn check(&self, target_pid: u32, app: &str) -> PrePaste {
            let t = Instant::now();
            if self.busy.swap(true, Ordering::SeqCst) {
                tracing::info!(%app, "pre-paste check still busy with the last one; pasting");
                return PrePaste::Paste;
            }
            let (reply, rx) = bounded(1);
            if self.tx.send(Request::Check { reply }).is_err() {
                self.busy.store(false, Ordering::SeqCst);
                return PrePaste::Paste;
            }
            let facts = match rx.recv_timeout(Duration::from_millis(PREPASTE_CHECK_MAX_MS)) {
                Ok(f) => f,
                Err(_) => {
                    tracing::info!(%app, ms = t.elapsed().as_millis() as u64, "pre-paste check too slow; pasting");
                    return PrePaste::Paste;
                }
            };
            let decision = decide(facts.as_ref(), target_pid, app);
            tracing::info!(
                %app,
                ms = t.elapsed().as_millis() as u64,
                role = facts.as_ref().map(|f| f.role.as_str()).unwrap_or("none"),
                writable_value = facts.as_ref().is_some_and(|f| f.value_settable),
                decision = ?decision,
                "pre-paste check"
            );
            decision
        }
    }
}

pub use platform::{Checker, decide};

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::watcher::uia::FocusFacts;
    use windows::Win32::UI::Accessibility::*;

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
            (chrome(UIA_EditControlTypeId, Some(false)), "msedge.exe", Paste, "text box"),
            (chrome(UIA_EditControlTypeId, None), "msedge.exe", Paste, "edit without value"),
            (chrome(UIA_EditControlTypeId, Some(true)), "msedge.exe", Paste, "read-only edit: unsure"),
            (chrome(UIA_DocumentControlTypeId, Some(true)), "msedge.exe", Paste, "page document: unsure"),
            (chrome(UIA_PaneControlTypeId, None), "claude.exe", Paste, "Chromium before accessibility is on"),
            (native(UIA_WindowControlTypeId, "Win32", None, false), "game.exe", Paste, "window"),
            (native(UIA_CustomControlTypeId, "", None, false), "app.exe", Paste, "custom"),
            (chrome(UIA_GroupControlTypeId, Some(false)), "msedge.exe", Paste, "editable group (writable value)"),
            (wt.clone(), "windowsterminal.exe", Paste, "Windows Terminal"),
            (wt, "someotherhost.exe", Paste, "TermControl hosted elsewhere"),
            (native(UIA_DocumentControlTypeId, "Win32", None, true), "conhost.exe", Paste, "conhost"),
            (native(UIA_ButtonControlTypeId, "Win32", None, false), "powershell.exe", Paste, "terminal"),
            (native(UIA_DataItemControlTypeId, "Win32", None, false), "excel.exe", Paste, "Excel cell"),
            (native(UIA_DataGridControlTypeId, "WPF", None, false), "app.exe", Paste, "data grid"),
            (native(UIA_TableControlTypeId, "WPF", None, false), "app.exe", Paste, "table"),
            (chrome(UIA_DataItemControlTypeId, None), "msedge.exe", Paste, "grid cell in a web app"),
            (native(UIA_TextControlTypeId, "WPF", None, true), "app.exe", Paste, "native text with a text pattern"),
            (chrome(UIA_GroupControlTypeId, Some(true)), "msedge.exe", NotEditable, "Chromium plain group"),
            (chrome(UIA_ButtonControlTypeId, None), "msedge.exe", NotEditable, "Chromium button"),
            (chrome(UIA_HyperlinkControlTypeId, Some(true)), "msedge.exe", NotEditable, "Chromium link"),
            (chrome(UIA_ListItemControlTypeId, None), "claude.exe", NotEditable, "Chromium list item"),
            (native(UIA_ListItemControlTypeId, "Win32", None, false), "explorer.exe", NotEditable, "desktop icon"),
            (native(UIA_ButtonControlTypeId, "Win32", None, false), "app.exe", NotEditable, "native button"),
            (native(UIA_TextControlTypeId, "Win32", None, false), "app.exe", NotEditable, "static label"),
        ];
        for (f, process, want, what) in table {
            assert_eq!(decide(Some(f), 10, process), *want, "{what}");
        }
    }

    #[test]
    fn no_answer_or_another_process_pastes() {
        assert_eq!(decide(None, 10, "app.exe"), PrePaste::Paste);
        let button = chrome(UIA_ButtonControlTypeId, None);
        assert_eq!(decide(Some(&button), 11, "msedge.exe"), PrePaste::Paste);
        assert_eq!(decide(Some(&FocusFacts { pid: 0, ..button.clone() }), 10, "msedge.exe"), PrePaste::Paste);
        assert_eq!(decide(Some(&FocusFacts { control_type: 0, ..button }), 10, "msedge.exe"), PrePaste::Paste);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::watcher::ax_macos::FocusFacts;

    fn ax(role: &str, settable: bool) -> FocusFacts {
        FocusFacts { pid: 10, role: role.into(), subrole: String::new(), value_settable: settable, has_value: settable }
    }

    #[test]
    fn only_definite_non_text_macos_roles_are_held_back() {
        use PrePaste::*;
        assert_eq!(decide(Some(&ax("AXTextField", true)), 10, "Notes"), Paste);
        assert_eq!(decide(Some(&ax("AXTextArea", true)), 10, "Claude"), Paste);
        assert_eq!(decide(Some(&ax("AXWebArea", false)), 10, "Safari"), Paste);
        assert_eq!(decide(Some(&ax("AXGroup", false)), 10, "Claude"), Paste);
        assert_eq!(decide(Some(&ax("AXButton", false)), 10, "Terminal"), Paste);
        assert_eq!(decide(Some(&ax("AXButton", false)), 10, "Warp"), Paste);
        assert_eq!(decide(Some(&ax("AXButton", false)), 10, "Safari"), NotEditable);
        assert_eq!(decide(Some(&ax("AXStaticText", false)), 10, "Finder"), NotEditable);
        assert_eq!(decide(Some(&ax("AXLink", false)), 10, "Google Chrome"), NotEditable);
        assert_eq!(decide(Some(&ax("AXCheckBox", false)), 10, "System Settings"), NotEditable);
        // Settable value always pastes even if the role looks unusual.
        assert_eq!(decide(Some(&ax("AXStaticText", true)), 10, "Notes"), Paste);
        // Unknown / mismatched PID / no answer -> paste.
        assert_eq!(decide(None, 10, "Safari"), Paste);
        assert_eq!(decide(Some(&ax("AXButton", false)), 11, "Safari"), Paste);
    }
}
