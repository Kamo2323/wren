//! The `WH_KEYBOARD_LL` hook thread (spec §3).
//!
//! One dedicated thread owns everything here: a hidden top-level window (for session and power
//! notifications, timers and our own control messages), the hook handle and the chord machine.
//! Because the machine lives on this thread and only this thread touches it, no locks exist.
//!
//! The hook callback only classifies the event, feeds the machine and pushes its outputs onto a
//! channel read by another thread. It never logs, does I/O or injects input, and it always ends
//! with `CallNextHookEx`: Wren never swallows a key.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use crossbeam_channel::Sender;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HC_ACTION,
    HHOOK, KBDLLHOOKSTRUCT, KillTimer, LLKHF_INJECTED, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PostQuitMessage, RegisterClassW, SetTimer,
    SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, WINDOW_EX_STYLE, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_POWERBROADCAST, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT,
    WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::w;

use super::chord::{ChordMachine, ChordOutput, KeyDir, KeyEvent};
use super::key_is_down;
use crate::constants::{
    HOOK_SILENT_REINSTALL_MS, HOOK_WATCHDOG_POLL_MS, INJECT_TAG, KEYSTATE_POLL_MS, SESSION_NOTIFY_RETRIES,
    SESSION_NOTIFY_RETRY_MS, VK_CHORD_CTRL, VK_CHORD_WIN,
};

/// Control messages posted to the hook window from other threads.
pub(super) const WM_WREN_REINSTALL: u32 = WM_APP + 1;
pub(super) const WM_WREN_FORCE_IDLE: u32 = WM_APP + 2;
pub(super) const WM_WREN_QUIT: u32 = WM_APP + 3;

const TIMER_KEYSTATE: usize = 1;
const TIMER_WATCHDOG: usize = 2;
const TIMER_SESSION_RETRY: usize = 3;

/// AltGr is delivered as a synthetic Left Ctrl down (this scan code, 0x1D with the 0x200 marker)
/// followed by Right Alt. That fake Ctrl must never count as the chord's Left Ctrl.
const ALTGR_FAKE_LCTRL_SCAN: u32 = 0x21D;

/// Why the hook is being reinstalled (decides how loudly it is logged).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reinstall {
    /// Periodic watchdog: can fire every few seconds under mouse-only use, so logged at debug.
    Watchdog,
    /// Unlock, resume or reconnect.
    Resume,
    /// `HotkeyService::reinstall` (tray "Reinstall hotkey").
    Manual,
}

/// Turns one hook callback into a machine input. `None` for messages that are not key down/up
/// and for AltGr's synthetic Left Ctrl. Pure, so it is unit-tested.
fn key_event_from(kb: &KBDLLHOOKSTRUCT, msg: u32, t_ms: u64) -> Option<KeyEvent> {
    let dir = match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => KeyDir::Down,
        WM_KEYUP | WM_SYSKEYUP => KeyDir::Up,
        _ => return None,
    };
    let vk = kb.vkCode as u16;
    if vk == VK_CHORD_CTRL && kb.scanCode == ALTGR_FAKE_LCTRL_SCAN {
        return None;
    }
    Some(KeyEvent {
        vk,
        dir,
        injected: (kb.flags.0 & LLKHF_INJECTED.0) != 0,
        tagged: kb.dwExtraInfo == INJECT_TAG,
        t_ms,
    })
}

/// Spec §3 watchdog rule on `GetTickCount` values (which wrap every ~49.7 days): reinstall when
/// Windows saw input at least `HOOK_SILENT_REINSTALL_MS` after the hook's last event. An input
/// time that appears to be in the future is ignored.
fn should_reinstall(last_input_tick: u32, last_hook_tick: u32, now_tick: u32) -> bool {
    let input_after_hook = last_input_tick.wrapping_sub(last_hook_tick) as i32;
    let input_age = now_tick.wrapping_sub(last_input_tick) as i32;
    input_age >= 0 && input_after_hook >= HOOK_SILENT_REINSTALL_MS as i32
}

/// State shared (read-mostly, lock-free) between the hook thread and `HotkeyService`.
pub(super) struct Shared {
    anchor: Instant,
    pub paused: AtomicBool,
    /// Milliseconds since `anchor` of the last event the hook saw (any key, tagged or not).
    last_event_ms: AtomicU64,
    /// `GetTickCount` of the last hook event; same clock as `GetLastInputInfo`.
    last_event_tick: AtomicU32,
    pub reinstalls: AtomicU64,
}

impl Shared {
    pub fn new() -> Self {
        Self {
            anchor: Instant::now(),
            paused: AtomicBool::new(false),
            last_event_ms: AtomicU64::new(0),
            last_event_tick: AtomicU32::new(unsafe { GetTickCount() }),
            reinstalls: AtomicU64::new(0),
        }
    }

    pub fn now_ms(&self) -> u64 {
        self.anchor.elapsed().as_millis() as u64
    }

    pub fn last_event_age_ms(&self) -> u64 {
        self.now_ms().saturating_sub(self.last_event_ms.load(Ordering::Relaxed))
    }
}

/// Everything the hook thread owns.
struct HookThread {
    machine: ChordMachine,
    out: Sender<ChordOutput>,
    shared: Arc<Shared>,
    hwnd: HWND,
    hinstance: HINSTANCE,
    hook: Option<HHOOK>,
    polling: bool,
    /// Lock/unlock notifications registered (retried by `TIMER_SESSION_RETRY` if not).
    session_registered: bool,
    session_retries_left: u32,
}

thread_local! {
    static STATE: RefCell<Option<HookThread>> = const { RefCell::new(None) };
}

/// Runs `f` on this thread's hook state. Skips (rather than panics) if the state is missing or
/// already borrowed, which can only happen during teardown.
fn with_state(f: impl FnOnce(&mut HookThread)) {
    STATE.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut()
            && let Some(state) = guard.as_mut()
        {
            f(state);
        }
    });
}

impl HookThread {
    fn emit(&self, outputs: Vec<ChordOutput>) {
        for o in outputs {
            let _ = self.out.send(o);
        }
    }

    /// Polls the chord keys only while recording (spec: lost key-ups).
    fn sync_keystate_timer(&mut self) {
        let want = self.machine.is_recording();
        if want == self.polling {
            return;
        }
        unsafe {
            if want {
                // Runs inside the hook callback, so no logging: if the timer cannot be created,
                // `polling` stays false and the next event or message tries again.
                if SetTimer(Some(self.hwnd), TIMER_KEYSTATE, KEYSTATE_POLL_MS as u32, None) == 0 {
                    return;
                }
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_KEYSTATE);
            }
        }
        self.polling = want;
    }

    /// First attempt at lock/unlock notifications. At early autostart Terminal Services may not be
    /// up yet; then a timer retries (`retry_session_notifications`) without delaying the hook.
    fn register_session_notifications(&mut self) {
        match unsafe { WTSRegisterSessionNotification(self.hwnd, NOTIFY_FOR_THIS_SESSION) } {
            Ok(()) => self.session_registered = true,
            Err(err) => {
                let timer = unsafe {
                    SetTimer(Some(self.hwnd), TIMER_SESSION_RETRY, SESSION_NOTIFY_RETRY_MS as u32, None)
                };
                if timer == 0 {
                    tracing::warn!(%err, "session notifications unavailable; lock/unlock will not reset the hotkey");
                } else {
                    self.session_retries_left = SESSION_NOTIFY_RETRIES;
                    tracing::info!(%err, "session notifications not ready yet; retrying");
                }
            }
        }
    }

    fn retry_session_notifications(&mut self) {
        let ok = unsafe { WTSRegisterSessionNotification(self.hwnd, NOTIFY_FOR_THIS_SESSION) }.is_ok();
        self.session_retries_left = self.session_retries_left.saturating_sub(1);
        if ok || self.session_retries_left == 0 {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_SESSION_RETRY);
            }
        }
        if ok {
            self.session_registered = true;
            tracing::info!("session notifications registered after retrying");
        } else if self.session_retries_left == 0 {
            tracing::warn!("session notifications unavailable; lock/unlock will not reset the hotkey");
        }
    }

    /// Installs a fresh hook *before* removing the old one, so there is no moment without a hook.
    /// (Both run on this thread, so no key event can be processed between the two calls.)
    fn install(&mut self) -> windows::core::Result<()> {
        let new = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_keyboard_proc), Some(self.hinstance), 0)? };
        if let Some(old) = self.hook.replace(new) {
            unsafe {
                let _ = UnhookWindowsHookEx(old);
            }
        }
        self.shared.last_event_tick.store(unsafe { GetTickCount() }, Ordering::Relaxed);
        Ok(())
    }

    fn reinstall(&mut self, why: Reinstall) {
        match self.install() {
            Ok(()) => {
                self.shared.reinstalls.fetch_add(1, Ordering::Relaxed);
                if why == Reinstall::Watchdog {
                    tracing::debug!(?why, "hotkey hook reinstalled");
                } else {
                    tracing::info!(?why, "hotkey hook reinstalled");
                }
            }
            Err(err) => tracing::error!(%err, ?why, "hotkey hook reinstall failed"),
        }
    }

    /// Re-reads physical key state: replays lost key-ups and forgets stuck keys.
    fn resync(&mut self) {
        let now = self.shared.now_ms();
        let out = self.machine.on_key_state(key_is_down(VK_CHORD_CTRL), key_is_down(VK_CHORD_WIN), now);
        self.machine.prune_other_keys(key_is_down);
        self.emit(out);
        self.sync_keystate_timer();
    }

    /// Lock, sleep or disconnect: cancel any recording (never a paste), forget all keys.
    fn interrupt(&mut self) {
        let out = self.machine.on_session_interrupted(self.shared.now_ms());
        self.emit(out);
        self.sync_keystate_timer();
    }

    /// Unlock, resume or reconnect: start clean with a fresh hook.
    fn resume(&mut self) {
        self.machine.force_idle();
        self.sync_keystate_timer();
        self.reinstall(Reinstall::Resume);
    }

    /// Spec §3 watchdog: Windows saw input well after the hook last saw anything.
    fn watchdog(&mut self) {
        let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return;
        }
        let last_hook = self.shared.last_event_tick.load(Ordering::Relaxed);
        if should_reinstall(info.dwTime, last_hook, unsafe { GetTickCount() }) {
            self.reinstall(Reinstall::Watchdog);
        }
    }

    /// The hook callback's work. Kept tiny: a few atomics, a few `GetAsyncKeyState`, channel sends.
    fn on_hook_event(&mut self, kb: &KBDLLHOOKSTRUCT, msg: u32) {
        // Any callback proves the hook is alive (for the watchdog), even one we then ignore.
        let t = self.shared.now_ms();
        self.shared.last_event_ms.store(t, Ordering::Relaxed);
        self.shared.last_event_tick.store(unsafe { GetTickCount() }, Ordering::Relaxed);
        let Some(ev) = key_event_from(kb, msg, t) else {
            return;
        };

        if ev.tagged || self.shared.paused.load(Ordering::Relaxed) {
            return; // the machine ignores tagged events anyway; skip the syscalls
        }
        let (vk, dir) = (ev.vk, ev.dir);

        let mut out = Vec::new();
        if dir == KeyDir::Down && (vk == VK_CHORD_CTRL || vk == VK_CHORD_WIN) {
            // Before a chord key goes down, trust the physical state of everything else: a lost
            // key-up must not turn a lone Win press into the chord, or block the chord forever.
            let partner = if vk == VK_CHORD_CTRL { VK_CHORD_WIN } else { VK_CHORD_CTRL };
            out.extend(self.machine.sync_key(partner, key_is_down(partner), t));
            self.machine.prune_other_keys(key_is_down);
        }
        out.extend(self.machine.on_key(ev));
        self.emit(out);
        self.sync_keystate_timer();
    }
}

unsafe extern "system" fn ll_keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL with HC_ACTION, lParam points to a KBDLLHOOKSTRUCT.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let msg = wparam.0 as u32;
        with_state(|s| s.on_hook_event(kb, msg));
    }
    // Always pass the key on. The hook handle argument is ignored by Windows.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_WREN_REINSTALL => with_state(|s| {
            s.reinstall(Reinstall::Manual);
            s.resync();
        }),
        WM_WREN_FORCE_IDLE => with_state(|s| {
            s.machine.force_idle();
            s.sync_keystate_timer();
        }),
        WM_WREN_QUIT => unsafe { PostQuitMessage(0) },
        WM_TIMER if wparam.0 == TIMER_KEYSTATE => with_state(|s| {
            let now = s.shared.now_ms();
            let mut out = s.machine.on_key_state(key_is_down(VK_CHORD_CTRL), key_is_down(VK_CHORD_WIN), now);
            out.extend(s.machine.on_tick(now));
            s.emit(out);
            s.sync_keystate_timer();
        }),
        WM_TIMER if wparam.0 == TIMER_WATCHDOG => with_state(|s| s.watchdog()),
        WM_TIMER if wparam.0 == TIMER_SESSION_RETRY => with_state(|s| s.retry_session_notifications()),
        WM_WTSSESSION_CHANGE => {
            let reason = wparam.0 as u32;
            if reason == WTS_SESSION_LOCK || reason == WTS_CONSOLE_DISCONNECT || reason == WTS_REMOTE_DISCONNECT {
                with_state(|s| s.interrupt());
            } else if reason == WTS_SESSION_UNLOCK || reason == WTS_CONSOLE_CONNECT || reason == WTS_REMOTE_CONNECT {
                with_state(|s| s.resume());
            }
        }
        WM_POWERBROADCAST => {
            let event = wparam.0 as u32;
            if event == PBT_APMSUSPEND {
                with_state(|s| s.interrupt());
            } else if event == PBT_APMRESUMESUSPEND || event == PBT_APMRESUMEAUTOMATIC {
                with_state(|s| s.resume());
            }
            return LRESULT(1);
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

/// Body of the hook thread. Sends the hook window handle (or an error) through `ready`, then
/// pumps messages until `WM_WREN_QUIT`.
pub(super) fn run(out: Sender<ChordOutput>, shared: Arc<Shared>, ready: Sender<Result<isize, String>>) {
    // Windows drops a low-level hook whose callback is slow; a busy machine must not starve it.
    if let Err(err) = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) } {
        tracing::warn!(%err, "could not raise hotkey thread priority");
    }
    let hwnd = match create_window() {
        Ok(v) => v,
        Err(err) => {
            let _ = ready.send(Err(format!("hotkey window: {err}")));
            return;
        }
    };
    let (hwnd, hinstance) = hwnd;
    let mut state = HookThread {
        machine: ChordMachine::new(),
        out,
        shared,
        hwnd,
        hinstance,
        hook: None,
        polling: false,
        session_registered: false,
        session_retries_left: 0,
    };
    // Everything else first; the hook goes in last, immediately before the message loop starts
    // pumping, so no key ever waits on a thread that is not yet reading messages.
    if unsafe { SetTimer(Some(hwnd), TIMER_WATCHDOG, HOOK_WATCHDOG_POLL_MS as u32, None) } == 0 {
        tracing::warn!("hotkey watchdog timer unavailable; a silently dropped hook will not be reinstalled");
    }
    state.register_session_notifications();
    if let Err(err) = state.install() {
        unsafe {
            if state.session_registered {
                let _ = WTSUnRegisterSessionNotification(hwnd);
            }
            let _ = DestroyWindow(hwnd);
        }
        let _ = ready.send(Err(format!("SetWindowsHookExW: {err}")));
        return;
    }
    STATE.with(|cell| *cell.borrow_mut() = Some(state));
    let _ = ready.send(Ok(hwnd.0 as isize));
    tracing::info!("hotkey hook installed");

    let mut msg = MSG::default();
    loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT (0) or error (-1)
        }
        unsafe {
            DispatchMessageW(&msg);
        }
    }

    // Teardown: take the state out first so re-entrant window messages find nothing to do.
    let state = STATE.with(|cell| cell.borrow_mut().take());
    if let Some(state) = state {
        unsafe {
            if let Some(h) = state.hook {
                let _ = UnhookWindowsHookEx(h);
            }
            if state.session_registered {
                let _ = WTSUnRegisterSessionNotification(state.hwnd);
            }
            let _ = KillTimer(Some(state.hwnd), TIMER_WATCHDOG);
            let _ = KillTimer(Some(state.hwnd), TIMER_KEYSTATE);
            let _ = KillTimer(Some(state.hwnd), TIMER_SESSION_RETRY);
            let _ = DestroyWindow(state.hwnd);
        }
        // Dropping `state.out` lets the dispatcher thread finish.
    }
    tracing::info!("hotkey hook removed");
}

/// A hidden, unowned top-level window: unowned top-level windows receive `WM_POWERBROADCAST`
/// (message-only windows do not).
fn create_window() -> windows::core::Result<(HWND, HINSTANCE)> {
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = w!("WrenHotkeyWindow");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinstance, lpszClassName: class, ..Default::default() };
        // Fails harmlessly with "class already exists" if a previous service registered it.
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
            class,
            w!("Wren hotkey"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )?;
        Ok((hwnd, hinstance))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::KBDLLHOOKSTRUCT_FLAGS;

    const SILENT: u32 = HOOK_SILENT_REINSTALL_MS as u32;

    fn kb(vk: u16, scan: u32, flags: u32, extra: usize) -> KBDLLHOOKSTRUCT {
        KBDLLHOOKSTRUCT {
            vkCode: vk as u32,
            scanCode: scan,
            flags: KBDLLHOOKSTRUCT_FLAGS(flags),
            time: 0,
            dwExtraInfo: extra,
        }
    }

    #[test]
    fn altgr_fake_left_ctrl_is_not_a_chord_key() {
        for msg in [WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP] {
            assert_eq!(key_event_from(&kb(VK_CHORD_CTRL, ALTGR_FAKE_LCTRL_SCAN, 0, 0), msg, 5), None, "msg {msg:#x}");
        }
    }

    #[test]
    fn altgr_with_win_held_never_starts_a_hold() {
        // Win held, then AltGr: Windows sends fake LCtrl (scan 0x21D) + Right Alt.
        let mut m = ChordMachine::new();
        let mut out = Vec::new();
        for (vk, scan, msg, t) in [
            (VK_CHORD_WIN, 0x5B, WM_KEYDOWN, 0),
            (VK_CHORD_CTRL, ALTGR_FAKE_LCTRL_SCAN, WM_KEYDOWN, 100),
            (0xA5, 0x38, WM_SYSKEYDOWN, 100),
            (VK_CHORD_CTRL, ALTGR_FAKE_LCTRL_SCAN, WM_KEYUP, 300),
            (0xA5, 0x38, WM_KEYUP, 300),
            (VK_CHORD_WIN, 0x5B, WM_KEYUP, 400),
        ] {
            if let Some(ev) = key_event_from(&kb(vk, scan, 0, 0), msg, t) {
                out.extend(m.on_key(ev));
            }
        }
        assert_eq!(out, vec![]);
    }

    #[test]
    fn real_left_ctrl_and_flags_are_classified() {
        let ev = key_event_from(&kb(VK_CHORD_CTRL, 0x1D, 0, 0), WM_KEYDOWN, 7).unwrap();
        assert_eq!(ev, KeyEvent { vk: VK_CHORD_CTRL, dir: KeyDir::Down, injected: false, tagged: false, t_ms: 7 });
        let ev = key_event_from(&kb(VK_CHORD_WIN, 0x5B, LLKHF_INJECTED.0, INJECT_TAG), WM_KEYUP, 9).unwrap();
        assert_eq!(ev, KeyEvent { vk: VK_CHORD_WIN, dir: KeyDir::Up, injected: true, tagged: true, t_ms: 9 });
        let ev = key_event_from(&kb(0x53, 0x1F, LLKHF_INJECTED.0, 0), WM_SYSKEYDOWN, 1).unwrap();
        assert!(ev.injected && !ev.tagged && ev.dir == KeyDir::Down);
        // Another app's tag is not ours.
        assert!(!key_event_from(&kb(0x53, 0x1F, 0, 0x5749_4C4C), WM_KEYDOWN, 1).unwrap().tagged);
        // Not a key message.
        assert_eq!(key_event_from(&kb(0x53, 0x1F, 0, 0), WM_TIMER, 1), None);
    }

    #[test]
    fn watchdog_reinstalls_only_after_silent_period() {
        // Hook saw a key at 10 000; input at 10 000 + SILENT - 1 is not enough, + SILENT is.
        assert!(!should_reinstall(10_000 + SILENT - 1, 10_000, 20_000));
        assert!(should_reinstall(10_000 + SILENT, 10_000, 20_000));
        // No input since the hook's last event (or the hook is newer than the input).
        assert!(!should_reinstall(10_000, 10_000, 30_000));
        assert!(!should_reinstall(9_000, 10_000, 30_000));
        // Input stamped in the future (clock oddity) is ignored.
        assert!(!should_reinstall(40_000, 10_000, 30_000));
    }

    #[test]
    fn watchdog_maths_survives_tick_wrap() {
        // GetTickCount wraps at 2^32 ms (~49.7 days).
        let hook = u32::MAX - 1_000;
        assert!(should_reinstall(hook.wrapping_add(SILENT), hook, hook.wrapping_add(SILENT + 50)));
        assert!(!should_reinstall(hook.wrapping_add(SILENT - 1), hook, hook.wrapping_add(SILENT + 50)));
        // Hook event just after the wrap, input just before it: hook is newer, no reinstall.
        assert!(!should_reinstall(u32::MAX - 10, 20, 100));
        // Input after the wrap, "now" also after the wrap, input not in the future.
        assert!(should_reinstall(4_500, u32::MAX - 600, 4_600));
        assert!(!should_reinstall(4_500, u32::MAX - 600, 4_400));
    }
}
