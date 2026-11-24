//! macOS global `Fn + Control` hotkey via CoreGraphics `CGEventTap` (spec §3).
//!
//! Maps macOS `Left Control` -> `VK_CHORD_CTRL` and `Fn` -> `VK_CHORD_WIN` (plus any other key or
//! modifier into non-colliding `u16` codes) so the exact same pure [`ChordMachine`] runs on both
//! Windows and macOS with identical hold, tap, double-tap hands-free, and shortcut-passthrough rules.
//!
//! Privacy & safety: the tap callback never logs key values, never swallows any key event (always
//! returns the original `CGEventRef`), and hands all state transitions off to the session channel.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use crossbeam_channel::Sender;

use super::HotkeyEvent;
use super::chord::{ChordMachine, ChordOutput, KeyDir, KeyEvent};
use crate::constants::{INJECT_TAG, KEYSTATE_POLL_MS, VK_CHORD_CTRL, VK_CHORD_WIN};

// ---- CoreGraphics & CoreFoundation FFI (system frameworks, zero extra crate churn) ----

type CFAllocatorRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFBooleanRef = *const c_void;
type CGEventTapProxy = *mut c_void;
type CGEventRef = *mut c_void;
type CGEventMask = u64;
type CGEventFlags = u64;

const K_CG_SESSION_EVENT_TAP: u32 = 1;
const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
const K_CG_EVENT_TAP_OPTION_DEFAULT: u32 = 0;
const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;

const K_CG_EVENT_KEY_DOWN: u32 = 10;
const K_CG_EVENT_KEY_UP: u32 = 11;
const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
const K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;

const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;
const K_CG_EVENT_SOURCE_USER_DATA: u32 = 42;
const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: i32 = 0;

// CGEventFlags bits
const MASK_SHIFT: u64 = 1 << 17;
const MASK_CONTROL: u64 = 1 << 18;
const MASK_ALTERNATE: u64 = 1 << 19;
const MASK_COMMAND: u64 = 1 << 20;
const MASK_SECONDARY_FN: u64 = 1 << 23;
// Device-specific right-control bit from IOLLEvent.h (`NX_DEVICERCTLKEYMASK`).
const MASK_RIGHT_CONTROL: u64 = 0x0000_2000;

// Synthetic key codes fed into `ChordMachine` for non-chord modifiers/keys.
// Must never collide with `VK_CHORD_CTRL` (0xA2) or `VK_CHORD_WIN` (0x5B).
const OTHER_KEY_BASE: u16 = 0x1000;
const MOD_SHIFT_VK: u16 = 0x2001;
const MOD_OPTION_VK: u16 = 0x2002;
const MOD_COMMAND_VK: u16 = 0x2003;
const MOD_RCTRL_VK: u16 = 0x2004;

type CGEventTapCallBack = unsafe extern "C" fn(
    proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> CGEventFlags;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventSourceFlagsState(state_id: i32) -> CGEventFlags;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFBooleanTrue: CFBooleanRef;
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: CFStringRef;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
}

/// True when macOS has granted Wren Accessibility permission (required for the global `Fn + Control`
/// hotkey, `⌘V` paste injection, and the correction watcher). If `prompt` is true, macOS displays
/// its native Accessibility prompt when not yet trusted.
pub fn accessibility_trusted(prompt: bool) -> bool {
    unsafe {
        if !prompt {
            return AXIsProcessTrusted();
        }
        let key = kAXTrustedCheckOptionPrompt as *const c_void;
        let val = kCFBooleanTrue as *const c_void;
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key,
            &val,
            1,
            std::ptr::null(),
            std::ptr::null(),
        );
        if dict.is_null() {
            return AXIsProcessTrusted();
        }
        let trusted = AXIsProcessTrustedWithOptions(dict);
        CFRelease(dict as *const c_void);
        trusted
    }
}

/// Pure flag classification (unit-tested without touching CoreGraphics).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ModState {
    left_ctrl: bool,
    fn_key: bool,
    shift: bool,
    option: bool,
    command: bool,
    right_ctrl: bool,
}

impl ModState {
    fn from_flags(flags: CGEventFlags) -> Self {
        let right_ctrl = (flags & MASK_RIGHT_CONTROL) != 0;
        let left_ctrl = (flags & MASK_CONTROL) != 0 && !right_ctrl;
        Self {
            left_ctrl,
            fn_key: (flags & MASK_SECONDARY_FN) != 0,
            shift: (flags & MASK_SHIFT) != 0,
            option: (flags & MASK_ALTERNATE) != 0,
            command: (flags & MASK_COMMAND) != 0,
            right_ctrl,
        }
    }

    fn diff_events(self, next: ModState, t_ms: u64, tagged: bool) -> Vec<KeyEvent> {
        let mut out = Vec::new();
        let pairs = [
            (self.right_ctrl, next.right_ctrl, MOD_RCTRL_VK),
            (self.shift, next.shift, MOD_SHIFT_VK),
            (self.option, next.option, MOD_OPTION_VK),
            (self.command, next.command, MOD_COMMAND_VK),
            (self.left_ctrl, next.left_ctrl, VK_CHORD_CTRL),
            (self.fn_key, next.fn_key, VK_CHORD_WIN),
        ];
        for (was, now, vk) in pairs {
            if was != now {
                out.push(KeyEvent {
                    vk,
                    dir: if now { KeyDir::Down } else { KeyDir::Up },
                    injected: false,
                    tagged,
                    t_ms,
                });
            }
        }
        out
    }
}

fn current_mod_state() -> ModState {
    let flags = unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) };
    ModState::from_flags(flags)
}

/// True when neither `Left Control` nor `Fn` is physically held.
pub fn chord_keys_physically_up() -> bool {
    let s = current_mod_state();
    !s.left_ctrl && !s.fn_key
}

struct Shared {
    anchor: Instant,
    paused: AtomicBool,
    last_event_ms: AtomicU64,
    reinstalls: AtomicU64,
    stop: AtomicBool,
    run_loop: AtomicIsize,
}

impl Shared {
    fn new() -> Self {
        Self {
            anchor: Instant::now(),
            paused: AtomicBool::new(false),
            last_event_ms: AtomicU64::new(0),
            reinstalls: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            run_loop: AtomicIsize::new(0),
        }
    }

    fn now_ms(&self) -> u64 {
        self.anchor.elapsed().as_millis() as u64
    }

    fn last_event_age_ms(&self) -> u64 {
        self.now_ms().saturating_sub(self.last_event_ms.load(Ordering::Relaxed))
    }
}

enum Control {
    Event(KeyEvent),
    ForceIdle,
    Interrupt,
    Resume,
    Resync,
    Reinstall,
    Quit,
}

struct TapContext {
    tap: CFMachPortRef,
    shared: Arc<Shared>,
    ctrl_tx: Sender<Control>,
    mods: parking_lot::Mutex<ModState>,
}

unsafe extern "C" fn event_tap_callback(
    _proxy: CGEventTapProxy,
    event_type: u32,
    event: CGEventRef,
    user_info: *mut c_void,
) -> CGEventRef {
    if user_info.is_null() {
        return event;
    }
    let ctx = unsafe { &*(user_info as *const TapContext) };

    if event_type == K_CG_EVENT_TAP_DISABLED_BY_TIMEOUT || event_type == K_CG_EVENT_TAP_DISABLED_BY_USER_INPUT {
        if !ctx.tap.is_null() {
            unsafe { CGEventTapEnable(ctx.tap, true) };
        }
        ctx.shared.reinstalls.fetch_add(1, Ordering::Relaxed);
        let _ = ctx.ctrl_tx.try_send(Control::Resync);
        return event;
    }

    let t = ctx.shared.now_ms();
    ctx.shared.last_event_ms.store(t, Ordering::Relaxed);

    if ctx.shared.paused.load(Ordering::Relaxed) || event.is_null() {
        return event;
    }

    let user_data = unsafe { CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) };
    let tagged = user_data as usize == INJECT_TAG;
    if tagged {
        return event;
    }

    match event_type {
        K_CG_EVENT_FLAGS_CHANGED => {
            let flags = unsafe { CGEventGetFlags(event) };
            let next = ModState::from_flags(flags);
            let events = {
                let mut prev = ctx.mods.lock();
                let diff = prev.diff_events(next, t, false);
                *prev = next;
                diff
            };
            for ev in events {
                let _ = ctx.ctrl_tx.try_send(Control::Event(ev));
            }
        }
        K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP => {
            let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) } as u16;
            let vk = OTHER_KEY_BASE | (keycode & 0x0FFF);
            let dir = if event_type == K_CG_EVENT_KEY_DOWN { KeyDir::Down } else { KeyDir::Up };
            let _ = ctx.ctrl_tx.try_send(Control::Event(KeyEvent {
                vk,
                dir,
                injected: false,
                tagged: false,
                t_ms: t,
            }));
        }
        _ => {}
    }

    // Never swallow any key event.
    event
}

fn create_event_tap(ctx_ptr: *mut c_void) -> Option<CFMachPortRef> {
    let mask: CGEventMask =
        (1u64 << K_CG_EVENT_KEY_DOWN) | (1u64 << K_CG_EVENT_KEY_UP) | (1u64 << K_CG_EVENT_FLAGS_CHANGED);
    // Prefer Default (active) tap so a single Accessibility grant suffices; fall back to ListenOnly.
    for opts in [K_CG_EVENT_TAP_OPTION_DEFAULT, K_CG_EVENT_TAP_OPTION_LISTEN_ONLY] {
        let tap = unsafe {
            CGEventTapCreate(
                K_CG_SESSION_EVENT_TAP,
                K_CG_HEAD_INSERT_EVENT_TAP,
                opts,
                mask,
                event_tap_callback,
                ctx_ptr,
            )
        };
        if !tap.is_null() {
            return Some(tap);
        }
    }
    None
}

/// The running macOS hotkey service. Dropping it removes the tap and stops its threads.
pub struct HotkeyService {
    shared: Arc<Shared>,
    ctrl_tx: Sender<Control>,
    tap_thread: Option<JoinHandle<()>>,
    machine_thread: Option<JoinHandle<()>>,
}

impl HotkeyService {
    pub fn start(tx: Sender<HotkeyEvent>) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared::new());
        let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded::<Control>();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<Result<(), String>>(1);

        let tap_shared = shared.clone();
        let tap_ctrl_tx = ctrl_tx.clone();
        let tap_thread = std::thread::Builder::new()
            .name("wren-hotkey-tap".into())
            .spawn(move || run_tap_loop(tap_shared, tap_ctrl_tx, ready_tx))
            .context("spawn macOS hotkey tap thread")?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => {
                let _ = tap_thread.join();
                return Err(anyhow!(msg));
            }
            Err(_) => {
                let _ = tap_thread.join();
                return Err(anyhow!("macOS hotkey thread exited during start-up"));
            }
        }

        let machine_shared = shared.clone();
        let machine_thread = std::thread::Builder::new()
            .name("wren-hotkey-machine".into())
            .spawn(move || run_machine(machine_shared, ctrl_rx, tx))
            .context("spawn macOS hotkey machine thread")?;

        // Subscribe to macOS sleep/wake and screen lock/unlock notifications to cancel active
        // recordings cleanly, reset stuck modifier state, and recreate the tap after wake.
        register_system_notifications(shared.clone(), ctrl_tx.clone());

        Ok(Self {
            shared,
            ctrl_tx,
            tap_thread: Some(tap_thread),
            machine_thread: Some(machine_thread),
        })
    }

    pub fn reinstall(&self) {
        self.shared.reinstalls.fetch_add(1, Ordering::Relaxed);
        let _ = self.ctrl_tx.try_send(Control::Reinstall);
        // Wake the run loop so it recreates the event tap immediately.
        stop_run_loop(&self.shared);
    }

    pub fn force_idle(&self) {
        let _ = self.ctrl_tx.try_send(Control::ForceIdle);
    }

    pub fn last_event_age_ms(&self) -> u64 {
        self.shared.last_event_age_ms()
    }

    pub fn set_paused(&self, paused: bool) {
        self.shared.paused.store(paused, Ordering::Relaxed);
        self.force_idle();
    }

    pub fn reinstall_count(&self) -> u64 {
        self.shared.reinstalls.load(Ordering::Relaxed)
    }
}

impl Drop for HotkeyService {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let _ = self.ctrl_tx.try_send(Control::Quit);
        stop_run_loop(&self.shared);
        if let Some(h) = self.tap_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.machine_thread.take() {
            let _ = h.join();
        }
    }
}

fn stop_run_loop(shared: &Shared) {
    let rl = shared.run_loop.load(Ordering::SeqCst);
    if rl != 0 {
        unsafe { CFRunLoopStop(rl as CFRunLoopRef) };
    }
}

fn run_tap_loop(shared: Arc<Shared>, ctrl_tx: Sender<Control>, ready: Sender<Result<(), String>>) {
    let mut ready_opt = Some(ready);

    while !shared.stop.load(Ordering::SeqCst) {
        let ctx = Box::new(TapContext {
            tap: std::ptr::null_mut(),
            shared: shared.clone(),
            ctrl_tx: ctrl_tx.clone(),
            mods: parking_lot::Mutex::new(current_mod_state()),
        });
        let ctx_ptr = Box::into_raw(ctx);

        let Some(tap) = create_event_tap(ctx_ptr as *mut c_void) else {
            unsafe { drop(Box::from_raw(ctx_ptr)) };
            if let Some(r) = ready_opt.take() {
                let trusted = accessibility_trusted(false);
                let msg = if trusted {
                    "CGEventTapCreate failed even with Accessibility enabled".to_string()
                } else {
                    "Accessibility permission is needed for the Fn + Control hotkey".to_string()
                };
                let _ = r.send(Err(msg));
                return;
            }
            // Retry briefly if reinstalling failed temporarily (e.g. mid-wake).
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };

        unsafe {
            (*ctx_ptr).tap = tap;
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            let rl = CFRunLoopGetCurrent();
            CFRetain(rl as *const c_void);
            shared.run_loop.store(rl as isize, Ordering::SeqCst);
            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);

            if let Some(r) = ready_opt.take() {
                let _ = r.send(Ok(()));
                tracing::info!("macOS Fn + Control event tap installed");
            } else {
                tracing::info!("macOS Fn + Control event tap reinstalled");
            }

            CFRunLoopRun();

            shared.run_loop.store(0, Ordering::SeqCst);
            CFRunLoopRemoveSource(rl, source, kCFRunLoopCommonModes);
            CFMachPortInvalidate(tap);
            CFRelease(source as *const c_void);
            CFRelease(tap as *const c_void);
            CFRelease(rl as *const c_void);
            drop(Box::from_raw(ctx_ptr));
        }
    }
    tracing::info!("macOS hotkey event tap removed");
}

fn run_machine(shared: Arc<Shared>, ctrl_rx: crossbeam_channel::Receiver<Control>, tx: Sender<HotkeyEvent>) {
    let mut machine = ChordMachine::new();
    let poll = Duration::from_millis(KEYSTATE_POLL_MS);

    let emit = |outputs: Vec<ChordOutput>| {
        for o in outputs {
            if let Some(ev) = HotkeyEvent::from_output(o)
                && tx.try_send(ev).is_err()
            {
                tracing::warn!(?ev, "hotkey event dropped: session channel full or closed");
            }
        }
    };

    loop {
        let msg = if machine.is_recording() {
            match ctrl_rx.recv_timeout(poll) {
                Ok(m) => Some(m),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    let now = shared.now_ms();
                    let s = current_mod_state();
                    let mut out = machine.on_key_state(s.left_ctrl, s.fn_key, now);
                    out.extend(machine.on_tick(now));
                    emit(out);
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            ctrl_rx.recv().ok()
        };

        let Some(msg) = msg else { break };
        match msg {
            Control::Quit => break,
            Control::ForceIdle => machine.force_idle(),
            Control::Interrupt => {
                let out = machine.on_session_interrupted(shared.now_ms());
                emit(out);
            }
            Control::Resume | Control::Reinstall => {
                machine.force_idle();
            }
            Control::Resync => {
                let now = shared.now_ms();
                let s = current_mod_state();
                let out = machine.on_key_state(s.left_ctrl, s.fn_key, now);
                emit(out);
            }
            Control::Event(ev) => {
                if shared.paused.load(Ordering::Relaxed) {
                    continue;
                }
                let mut out = Vec::new();
                if ev.dir == KeyDir::Down && (ev.vk == VK_CHORD_CTRL || ev.vk == VK_CHORD_WIN) {
                    let s = current_mod_state();
                    let (partner, partner_down) = if ev.vk == VK_CHORD_CTRL {
                        (VK_CHORD_WIN, s.fn_key)
                    } else {
                        (VK_CHORD_CTRL, s.left_ctrl)
                    };
                    out.extend(machine.sync_key(partner, partner_down, ev.t_ms));
                }
                out.extend(machine.on_key(ev));
                emit(out);
            }
        }
    }
}

/// Listen for macOS sleep/wake and screen lock/unlock to cancel any active recording cleanly
/// and recreate the event tap after sleep wake.
fn register_system_notifications(shared: Arc<Shared>, ctrl_tx: Sender<Control>) {
    use block2::RcBlock;
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSString};

    let tx_sleep = ctrl_tx.clone();
    let tx_wake = ctrl_tx.clone();
    let wake_shared = shared.clone();
    let tx_lock = ctrl_tx.clone();
    let tx_unlock = ctrl_tx;
    let unlock_shared = shared;

    unsafe {
        let ws_center = NSWorkspace::sharedWorkspace().notificationCenter();
        let sleep_name = NSString::from_str("NSWorkspaceWillSleepNotification");
        let wake_name = NSString::from_str("NSWorkspaceDidWakeNotification");

        let sleep_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
            let _ = tx_sleep.try_send(Control::Interrupt);
        });
        let wake_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
            if !wake_shared.stop.load(Ordering::SeqCst) {
                wake_shared.reinstalls.fetch_add(1, Ordering::Relaxed);
                let _ = tx_wake.try_send(Control::Resume);
                stop_run_loop(&wake_shared);
            }
        });
        let _ = ws_center.addObserverForName_object_queue_usingBlock(Some(&sleep_name), None, None, &sleep_block);
        let _ = ws_center.addObserverForName_object_queue_usingBlock(Some(&wake_name), None, None, &wake_block);

        let dist_center = NSDistributedNotificationCenter::defaultCenter();
        let lock_name = NSString::from_str("com.apple.screenIsLocked");
        let unlock_name = NSString::from_str("com.apple.screenIsUnlocked");

        let lock_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
            let _ = tx_lock.try_send(Control::Interrupt);
        });
        let unlock_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
            if !unlock_shared.stop.load(Ordering::SeqCst) {
                let _ = tx_unlock.try_send(Control::Resume);
            }
        });
        let _ = dist_center.addObserverForName_object_queue_usingBlock(Some(&lock_name), None, None, &lock_block);
        let _ = dist_center.addObserverForName_object_queue_usingBlock(Some(&unlock_name), None, None, &unlock_block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_and_left_control_flags_drive_chord_machine() {
        let mut m = ChordMachine::new();
        let s0 = ModState::from_flags(0);
        let s1 = ModState::from_flags(MASK_SECONDARY_FN);
        let s2 = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL);
        let s3 = ModState::from_flags(MASK_SECONDARY_FN);
        let s4 = ModState::from_flags(0);

        let mut out = Vec::new();
        for ev in s0.diff_events(s1, 0, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.is_empty());
        for ev in s1.diff_events(s2, 50, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.contains(&ChordOutput::HoldStart));
        out.clear();
        for ev in s2.diff_events(s3, 900, false) {
            out.extend(m.on_key(ev));
        }
        assert_eq!(out, vec![ChordOutput::HoldEnd { held_ms: 850 }]);
        out.clear();
        for ev in s3.diff_events(s4, 920, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.is_empty());
    }

    #[test]
    fn fn_control_arrow_cancels_for_window_tiling() {
        let mut m = ChordMachine::new();
        let s0 = ModState::from_flags(0);
        let s1 = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL);
        let mut out = Vec::new();
        for ev in s0.diff_events(s1, 0, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.contains(&ChordOutput::HoldStart));
        // Left arrow keyCode 123 mapped above OTHER_KEY_BASE.
        let arrow = KeyEvent {
            vk: OTHER_KEY_BASE | 123,
            dir: KeyDir::Down,
            injected: false,
            tagged: false,
            t_ms: 80,
        };
        assert_eq!(m.on_key(arrow), vec![ChordOutput::Cancel { held_ms: 80 }]);
    }

    #[test]
    fn right_control_does_not_trigger_left_control_chord() {
        let s = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL | MASK_RIGHT_CONTROL);
        assert!(s.fn_key);
        assert!(!s.left_ctrl);
        assert!(s.right_ctrl);
    }

    #[test]
    fn extra_modifier_blocks_chord() {
        let mut m = ChordMachine::new();
        let s0 = ModState::from_flags(0);
        let s1 = ModState::from_flags(MASK_COMMAND);
        let s2 = ModState::from_flags(MASK_COMMAND | MASK_SECONDARY_FN | MASK_CONTROL);
        let mut out = Vec::new();
        for ev in s0.diff_events(s1, 0, false) {
            out.extend(m.on_key(ev));
        }
        for ev in s1.diff_events(s2, 40, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.is_empty(), "Cmd + Fn + Control must never start recording: {out:?}");
    }
}
