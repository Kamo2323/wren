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
    HOOK_SILENT_REINSTALL_MS, HOOK_WATCHDOG_POLL_MS, INJECT_TAG, KEYSTATE_POLL_MS, VK_CHORD_CTRL, VK_CHORD_WIN,
};

/// Control messages posted to the hook window from other threads.
pub(super) const WM_WREN_REINSTALL: u32 = WM_APP + 1;
pub(super) const WM_WREN_FORCE_IDLE: u32 = WM_APP + 2;
pub(super) const WM_WREN_QUIT: u32 = WM_APP + 3;

const TIMER_KEYSTATE: usize = 1;
const TIMER_WATCHDOG: usize = 2;

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
                SetTimer(Some(self.hwnd), TIMER_KEYSTATE, KEYSTATE_POLL_MS as u32, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_KEYSTATE);
            }
        }
        self.polling = want;
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

    fn reinstall(&mut self) {
        match self.install() {
            Ok(()) => {
                self.shared.reinstalls.fetch_add(1, Ordering::Relaxed);
                tracing::info!("hotkey hook reinstalled");
            }
            Err(err) => tracing::error!(%err, "hotkey hook reinstall failed"),
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

    /// Lock, sleep or disconnect: cancel a hold / stop hands-free, forget all keys.
    fn interrupt(&mut self) {
        let out = self.machine.on_session_interrupted(self.shared.now_ms());
        self.emit(out);
        self.sync_keystate_timer();
    }

    /// Unlock, resume or reconnect: start clean with a fresh hook.
    fn resume(&mut self) {
        self.machine.force_idle();
        self.sync_keystate_timer();
        self.reinstall();
    }

    /// Spec §3 watchdog: Windows saw input well after the hook last saw anything.
    fn watchdog(&mut self) {
        let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return;
        }
        let last_hook = self.shared.last_event_tick.load(Ordering::Relaxed);
        let input_after_hook = info.dwTime.wrapping_sub(last_hook) as i32;
        if input_after_hook >= HOOK_SILENT_REINSTALL_MS as i32 {
            self.reinstall();
        }
    }

    /// The hook callback's work. Kept tiny: a few atomics, a few `GetAsyncKeyState`, channel sends.
    fn on_hook_event(&mut self, kb: &KBDLLHOOKSTRUCT, msg: u32) {
        let dir = match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => KeyDir::Down,
            WM_KEYUP | WM_SYSKEYUP => KeyDir::Up,
            _ => return,
        };
        let t = self.shared.now_ms();
        self.shared.last_event_ms.store(t, Ordering::Relaxed);
        self.shared.last_event_tick.store(unsafe { GetTickCount() }, Ordering::Relaxed);

        let tagged = kb.dwExtraInfo == INJECT_TAG;
        if tagged || self.shared.paused.load(Ordering::Relaxed) {
            return; // the machine ignores tagged events anyway; skip the syscalls
        }
        let vk = kb.vkCode as u16;
        let ev = KeyEvent { vk, dir, injected: (kb.flags.0 & LLKHF_INJECTED.0) != 0, tagged, t_ms: t };

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
            s.reinstall();
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
    };
    if let Err(err) = state.install() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        let _ = ready.send(Err(format!("SetWindowsHookExW: {err}")));
        return;
    }
    unsafe {
        if let Err(err) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            tracing::warn!(%err, "session notifications unavailable; lock/unlock will not reset the hotkey");
        }
        SetTimer(Some(hwnd), TIMER_WATCHDOG, HOOK_WATCHDOG_POLL_MS as u32, None);
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
            let _ = WTSUnRegisterSessionNotification(state.hwnd);
            let _ = KillTimer(Some(state.hwnd), TIMER_WATCHDOG);
            let _ = KillTimer(Some(state.hwnd), TIMER_KEYSTATE);
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
