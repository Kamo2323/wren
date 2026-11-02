//! Global Ctrl+Win hotkey (spec §3).
//!
//! `HotkeyService::start` spawns two threads:
//! - the hook thread (`hook.rs`): low-level keyboard hook, chord machine, watchdog;
//! - the dispatcher: injects the Start-menu mask on `MaskNow` and forwards everything else to
//!   the session as `HotkeyEvent`s. Injection never happens inside the hook callback.

pub mod chord;
mod hook;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;

use anyhow::{Context, anyhow};
use crossbeam_channel::Sender;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, MapVirtualKeyW, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::constants::{INJECT_TAG, VK_CHORD_CTRL, VK_CHORD_WIN, VK_MASK};
use chord::ChordOutput;

/// What the session hears from the hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    /// Chord pressed: start recording now (the first word must not be clipped).
    HoldStart,
    /// Chord released after a real hold: transcribe and paste.
    HoldEnd { held_ms: u64 },
    /// Chord released too quickly: discard the audio.
    Tap,
    /// Second press of a double-tap: start a hands-free recording.
    HandsFreeStart,
    /// Next chord press during hands-free: stop, transcribe and paste.
    HandsFreeStop,
    /// Discard: another key joined a held chord (a Windows shortcut), or the session locked, slept
    /// or disconnected during any recording (hold or hands-free). Never pasted; saved to History
    /// when `held_ms` ≥ `CANCEL_SAVE_AFTER_MS`. For hands-free, `held_ms` counts from its start.
    Cancel { held_ms: u64 },
}

impl HotkeyEvent {
    fn from_output(o: ChordOutput) -> Option<Self> {
        Some(match o {
            ChordOutput::MaskNow => return None,
            ChordOutput::HoldStart => Self::HoldStart,
            ChordOutput::HoldEnd { held_ms } => Self::HoldEnd { held_ms },
            ChordOutput::Tap => Self::Tap,
            ChordOutput::HandsFreeStart => Self::HandsFreeStart,
            ChordOutput::HandsFreeStop => Self::HandsFreeStop,
            ChordOutput::Cancel { held_ms } => Self::Cancel { held_ms },
        })
    }
}

/// The running hotkey. Dropping it removes the hook and stops both threads.
pub struct HotkeyService {
    /// The hook thread's window, as an integer so the service is `Send + Sync`.
    hwnd: isize,
    shared: Arc<hook::Shared>,
    hook_thread: Option<JoinHandle<()>>,
    dispatch_thread: Option<JoinHandle<()>>,
}

impl HotkeyService {
    /// Installs the hook and starts sending events to `tx`.
    ///
    /// `tx` must be an **unbounded** channel (`crossbeam_channel::unbounded`). Events are sent
    /// with `try_send`, so with a bounded channel that fills up, events are dropped (and logged)
    /// rather than stalling the Start-menu mask or `Drop`.
    pub fn start(tx: Sender<HotkeyEvent>) -> anyhow::Result<Self> {
        let shared = Arc::new(hook::Shared::new());
        let (out_tx, out_rx) = crossbeam_channel::unbounded::<ChordOutput>();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);

        let hook_shared = shared.clone();
        let hook_thread = std::thread::Builder::new()
            .name("wren-hotkey".into())
            .spawn(move || hook::run(out_tx, hook_shared, ready_tx))
            .context("spawn hotkey thread")?;
        let hwnd = match ready_rx.recv() {
            Ok(Ok(hwnd)) => hwnd,
            Ok(Err(msg)) => {
                let _ = hook_thread.join();
                return Err(anyhow!(msg));
            }
            Err(_) => {
                let _ = hook_thread.join();
                return Err(anyhow!("hotkey thread exited during start-up"));
            }
        };

        let dispatch_thread = std::thread::Builder::new()
            .name("wren-hotkey-dispatch".into())
            .spawn(move || {
                // Ends when the hook thread drops its sender.
                while let Ok(first) = out_rx.recv() {
                    // Take everything already queued, forward the session's events first (so
                    // `HoldStart` starts audio without waiting on SendInput, which runs every other
                    // app's hooks), then inject the masks. Win is still held: the mask makes
                    // Windows treat the press as a shortcut, so Start stays shut.
                    let mut masks = 0usize;
                    for o in std::iter::once(first).chain(out_rx.try_iter()) {
                        match HotkeyEvent::from_output(o) {
                            None => masks += 1,
                            // Never block: a full (bounded) channel must not stall masks or Drop.
                            Some(ev) => {
                                if tx.try_send(ev).is_err() {
                                    tracing::warn!(?ev, "hotkey event dropped: session channel full or closed");
                                }
                            }
                        }
                    }
                    for _ in 0..masks {
                        send_tagged_keys(&[(VK_MASK, true), (VK_MASK, false)]);
                    }
                }
            })
            .context("spawn hotkey dispatcher")?;

        Ok(Self { hwnd, shared, hook_thread: Some(hook_thread), dispatch_thread: Some(dispatch_thread) })
    }

    /// Reinstalls the hook and re-reads physical key state (tray "Reinstall hotkey").
    pub fn reinstall(&self) {
        self.post(hook::WM_WREN_REINSTALL);
    }

    /// Drops whatever the chord machine was doing, with no event (the session stopped on its own).
    pub fn force_idle(&self) {
        self.post(hook::WM_WREN_FORCE_IDLE);
    }

    /// Milliseconds since the hook last saw any key event.
    pub fn last_event_age_ms(&self) -> u64 {
        self.shared.last_event_age_ms()
    }

    /// While paused the hook stays installed but the chord does nothing.
    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
        self.force_idle();
    }

    /// How many times the hook has been reinstalled (watchdog, resume, unlock, `reinstall`).
    pub fn reinstall_count(&self) -> u64 {
        self.shared.reinstalls.load(Ordering::Relaxed)
    }

    fn post(&self, msg: u32) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        self.post(hook::WM_WREN_QUIT);
        if let Some(h) = self.hook_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.dispatch_thread.take() {
            let _ = h.join();
        }
    }
}

pub(crate) fn key_is_down(vk: u16) -> bool {
    unsafe { GetAsyncKeyState(vk as i32) < 0 }
}

/// True when neither Left Ctrl nor Left Win is physically held.
pub fn chord_keys_physically_up() -> bool {
    !key_is_down(VK_CHORD_CTRL) && !key_is_down(VK_CHORD_WIN)
}

/// Injects the given key downs/ups in one `SendInput` call, tagged so Wren's own hook ignores them.
pub fn send_tagged_keys(keys: &[(u16, bool /*down*/)]) {
    send_keys_with_extra_info(keys, INJECT_TAG);
}

/// `send_tagged_keys` with any `dwExtraInfo`. Only the live probe uses a value other than the tag
/// (untagged keys drive the hotkey like a real keyboard).
#[doc(hidden)]
pub fn send_keys_with_extra_info(keys: &[(u16, bool)], extra_info: usize) {
    if keys.is_empty() {
        return;
    }
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, down)| {
            let mut flags = KEYBD_EVENT_FLAGS(0);
            if !down {
                flags |= KEYEVENTF_KEYUP;
            }
            if is_extended_key(vk) {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: extra_info },
                },
            }
        })
        .collect();
    let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        tracing::warn!(sent, wanted = inputs.len(), "SendInput was partly blocked");
    }
}

/// Keys that need `KEYEVENTF_EXTENDEDKEY` to be told apart from their numpad/left twins.
fn is_extended_key(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // PgUp, PgDn, End, Home, arrows
            | 0x2C // Print Screen
            | 0x2D | 0x2E // Insert, Delete
            | 0x5B | 0x5C | 0x5D // Left/Right Win, Apps
            | 0x6F // Numpad divide
            | 0x90 // Num Lock
            | 0xA3 | 0xA5 // Right Ctrl, Right Alt
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_events_map_one_to_one_and_mask_is_internal() {
        assert_eq!(HotkeyEvent::from_output(ChordOutput::MaskNow), None);
        assert_eq!(HotkeyEvent::from_output(ChordOutput::HoldStart), Some(HotkeyEvent::HoldStart));
        assert_eq!(
            HotkeyEvent::from_output(ChordOutput::HoldEnd { held_ms: 7 }),
            Some(HotkeyEvent::HoldEnd { held_ms: 7 })
        );
        assert_eq!(HotkeyEvent::from_output(ChordOutput::Tap), Some(HotkeyEvent::Tap));
        assert_eq!(HotkeyEvent::from_output(ChordOutput::HandsFreeStart), Some(HotkeyEvent::HandsFreeStart));
        assert_eq!(HotkeyEvent::from_output(ChordOutput::HandsFreeStop), Some(HotkeyEvent::HandsFreeStop));
        assert_eq!(
            HotkeyEvent::from_output(ChordOutput::Cancel { held_ms: 9 }),
            Some(HotkeyEvent::Cancel { held_ms: 9 })
        );
    }

    #[test]
    fn win_and_right_hand_modifiers_are_extended() {
        for vk in [0x5B, 0x5C, 0xA3, 0xA5, 0x25, 0x27] {
            assert!(is_extended_key(vk), "{vk:#x}");
        }
        for vk in [VK_CHORD_CTRL, VK_MASK, 0x56, 0xA0, 0xA4, 0x0D] {
            assert!(!is_extended_key(vk), "{vk:#x}");
        }
    }

    /// Starting and dropping the service must install and remove the hook cleanly, twice
    /// (the window class is registered once and reused). Installs a real system-wide hook, so it
    /// is opt-in: `cargo test -- --ignored service_starts_and_stops_cleanly`.
    #[test]
    #[ignore = "installs a real system-wide keyboard hook"]
    fn service_starts_and_stops_cleanly() {
        for _ in 0..2 {
            let (tx, _rx) = crossbeam_channel::unbounded();
            let svc = HotkeyService::start(tx).expect("hook installs");
            assert!(svc.last_event_age_ms() < 60_000);
            svc.reinstall();
            svc.set_paused(true);
            svc.set_paused(false);
            drop(svc);
        }
    }
}
