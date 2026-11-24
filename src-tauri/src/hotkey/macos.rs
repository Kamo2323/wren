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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use crossbeam_channel::Sender;
use parking_lot::Mutex;

use super::HotkeyEvent;
use super::chord::{ChordMachine, ChordOutput, KeyDir, KeyEvent};
use crate::constants::{INJECT_TAG, KEYSTATE_POLL_MS, VK_CHORD_CTRL, VK_CHORD_WIN};

// ---- CoreGraphics & CoreFoundation FFI (system frameworks, zero extra crate churn) ----

type CFAllocatorRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopTimerRef = *mut c_void;
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

// CGEventFlags bits (including device-specific left/right control bits from IOLLEvent.h).
const MASK_LEFT_CONTROL: u64 = 0x0000_0001;
const MASK_RIGHT_CONTROL: u64 = 0x0000_2000;
const MASK_SHIFT: u64 = 1 << 17;
const MASK_CONTROL: u64 = 1 << 18;
const MASK_ALTERNATE: u64 = 1 << 19;
const MASK_COMMAND: u64 = 1 << 20;
const MASK_SECONDARY_FN: u64 = 1 << 23;

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

type CFRunLoopTimerCallBack = unsafe extern "C" fn(timer: CFRunLoopTimerRef, info: *mut c_void);

#[repr(C)]
struct CFRunLoopTimerContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    copy_description: *const c_void,
}

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
    fn CGEventSourceKeyState(state_id: i32, key: u16) -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFBooleanTrue: CFBooleanRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopTimerCreate(
        allocator: CFAllocatorRef,
        fire_date: f64,
        interval: f64,
        flags: usize,
        order: isize,
        callout: CFRunLoopTimerCallBack,
        context: *mut CFRunLoopTimerContext,
    ) -> CFRunLoopTimerRef;
    fn CFRunLoopAddTimer(rl: CFRunLoopRef, timer: CFRunLoopTimerRef, mode: CFStringRef);
    fn CFRunLoopTimerInvalidate(timer: CFRunLoopTimerRef);
    fn CFRunLoopRun();
    fn CFRunLoopWakeUp(rl: CFRunLoopRef);
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
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> u8;
}

/// True when macOS has granted Wren Accessibility permission (required for the global `Fn + Control`
/// hotkey, `⌘V` paste injection, and the correction watcher). If `prompt` is true, macOS displays
/// its native Accessibility prompt when not yet trusted.
pub fn accessibility_trusted(prompt: bool) -> bool {
    unsafe {
        if !prompt {
            return AXIsProcessTrusted() != 0;
        }
        let key = kAXTrustedCheckOptionPrompt as *const c_void;
        let val = kCFBooleanTrue as *const c_void;
        let dict = CFDictionaryCreate(
            std::ptr::null(),
            &key,
            &val,
            1,
            &kCFTypeDictionaryKeyCallBacks as *const c_void,
            &kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        if dict.is_null() {
            return AXIsProcessTrusted() != 0;
        }
        let trusted = AXIsProcessTrustedWithOptions(dict) != 0;
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
        let left_dev = (flags & MASK_LEFT_CONTROL) != 0;
        // Real macOS CGEventFlags carry device-specific left/right control bits; when both
        // controls are held and right control releases, keeping left_ctrl true via MASK_LEFT_CONTROL
        // avoids synthesising a fake Left Control Down on Right Control release.
        let left_ctrl = if left_dev || right_ctrl {
            left_dev
        } else {
            (flags & MASK_CONTROL) != 0
        };
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

fn is_other_key_still_down(vk: u16, mods: ModState) -> bool {
    match vk {
        MOD_SHIFT_VK => mods.shift,
        MOD_OPTION_VK => mods.option,
        MOD_COMMAND_VK => mods.command,
        MOD_RCTRL_VK => mods.right_ctrl,
        code if code >= OTHER_KEY_BASE && code < MOD_SHIFT_VK => {
            let keycode = code & 0x0FFF;
            unsafe { CGEventSourceKeyState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION, keycode) != 0 }
        }
        _ => false,
    }
}

/// True when neither `Left Control` nor `Fn` is physically held.
pub fn chord_keys_physically_up() -> bool {
    let s = current_mod_state();
    !s.left_ctrl && !s.fn_key
}

/// Synchronises `CFRunLoopStop` against `CFRunLoopRun` teardown so `Drop` / `reinstall` can never
/// call `CFRunLoopStop` on a released run loop or race ahead of `CFRunLoopRun()` entry.
struct TapRunState {
    run_loop: Option<usize>,
    stop: bool,
    reinstall_seq: u64,
}

struct Shared {
    anchor: Instant,
    paused: AtomicBool,
    last_event_ms: AtomicU64,
    reinstalls: AtomicU64,
    tap_state: Mutex<TapRunState>,
}

impl Shared {
    fn new() -> Self {
        Self {
            anchor: Instant::now(),
            paused: AtomicBool::new(false),
            last_event_ms: AtomicU64::new(0),
            reinstalls: AtomicU64::new(0),
            tap_state: Mutex::new(TapRunState {
                run_loop: None,
                stop: false,
                reinstall_seq: 0,
            }),
        }
    }

    fn now_ms(&self) -> u64 {
        self.anchor.elapsed().as_millis() as u64
    }

    fn last_event_age_ms(&self) -> u64 {
        self.now_ms().saturating_sub(self.last_event_ms.load(Ordering::Relaxed))
    }

    fn is_stopped(&self) -> bool {
        self.tap_state.lock().stop
    }

    fn request_stop(&self) {
        let retained_rl = {
            let mut st = self.tap_state.lock();
            st.stop = true;
            st.run_loop.map(|rl| unsafe {
                CFRetain(rl as *const c_void);
                rl as CFRunLoopRef
            })
        };
        if let Some(rl) = retained_rl {
            unsafe {
                CFRunLoopWakeUp(rl);
                CFRunLoopStop(rl);
                CFRelease(rl as *const c_void);
            }
        }
    }

    fn request_reinstall(&self) {
        self.reinstalls.fetch_add(1, Ordering::Relaxed);
        let retained_rl = {
            let mut st = self.tap_state.lock();
            if st.stop {
                return;
            }
            st.reinstall_seq = st.reinstall_seq.wrapping_add(1);
            st.run_loop.map(|rl| unsafe {
                CFRetain(rl as *const c_void);
                rl as CFRunLoopRef
            })
        };
        if let Some(rl) = retained_rl {
            unsafe {
                CFRunLoopWakeUp(rl);
                CFRunLoopStop(rl);
                CFRelease(rl as *const c_void);
            }
        }
    }
}

enum Control {
    Event { ev: KeyEvent, mods: ModState },
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
    mods: Mutex<ModState>,
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
        *ctx.mods.lock() = current_mod_state();
        ctx.shared.reinstalls.fetch_add(1, Ordering::Relaxed);
        let _ = ctx.ctrl_tx.try_send(Control::Resync);
        return event;
    }

    let t = ctx.shared.now_ms();
    ctx.shared.last_event_ms.store(t, Ordering::Relaxed);

    if event.is_null() {
        return event;
    }

    let user_data = unsafe { CGEventGetIntegerValueField(event, K_CG_EVENT_SOURCE_USER_DATA) };
    let tagged = user_data as usize == INJECT_TAG;
    if tagged {
        return event;
    }

    let paused = ctx.shared.paused.load(Ordering::Relaxed);
    match event_type {
        K_CG_EVENT_FLAGS_CHANGED => {
            let flags = unsafe { CGEventGetFlags(event) };
            let next = ModState::from_flags(flags);
            let events = {
                let mut prev = ctx.mods.lock();
                let diff = if paused { Vec::new() } else { prev.diff_events(next, t, false) };
                *prev = next;
                diff
            };
            for ev in events {
                let _ = ctx.ctrl_tx.try_send(Control::Event { ev, mods: next });
            }
        }
        K_CG_EVENT_KEY_DOWN | K_CG_EVENT_KEY_UP => {
            if paused {
                return event;
            }
            let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) } as u16;
            let vk = OTHER_KEY_BASE | (keycode & 0x0FFF);
            let dir = if event_type == K_CG_EVENT_KEY_DOWN { KeyDir::Down } else { KeyDir::Up };
            let mods = *ctx.mods.lock();
            let _ = ctx.ctrl_tx.try_send(Control::Event {
                ev: KeyEvent {
                    vk,
                    dir,
                    injected: false,
                    tagged: false,
                    t_ms: t,
                },
                mods,
            });
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

/// Owned observer tokens removed from `NSWorkspace` / `NSDistributedNotificationCenter` on drop.
struct SystemObservers {
    ws_tokens: Vec<usize>,
    dist_tokens: Vec<usize>,
}

impl Drop for SystemObservers {
    fn drop(&mut self) {
        objc2::rc::autoreleasepool(|_| unsafe {
            let ws_center = objc2_app_kit::NSWorkspace::sharedWorkspace().notificationCenter();
            for raw in self.ws_tokens.drain(..) {
                let obj = objc2::rc::Retained::from_raw(raw as *mut objc2::runtime::AnyObject);
                if let Some(obs) = obj {
                    ws_center.removeObserver(&obs);
                }
            }
            let dist_center = objc2_foundation::NSDistributedNotificationCenter::defaultCenter();
            for raw in self.dist_tokens.drain(..) {
                let obj = objc2::rc::Retained::from_raw(raw as *mut objc2::runtime::AnyObject);
                if let Some(obs) = obj {
                    dist_center.removeObserver(&obs);
                }
            }
        });
    }
}

/// The running macOS hotkey service. Dropping it removes the tap, observers, and stops its threads.
pub struct HotkeyService {
    shared: Arc<Shared>,
    ctrl_tx: Sender<Control>,
    _observers: SystemObservers,
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

        let observers = register_system_notifications(shared.clone(), ctrl_tx.clone());

        Ok(Self {
            shared,
            ctrl_tx,
            _observers: observers,
            tap_thread: Some(tap_thread),
            machine_thread: Some(machine_thread),
        })
    }

    pub fn reinstall(&self) {
        let _ = self.ctrl_tx.try_send(Control::Reinstall);
        self.shared.request_reinstall();
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
        self.shared.request_stop();
        let _ = self.ctrl_tx.try_send(Control::Quit);
        if let Some(h) = self.tap_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.machine_thread.take() {
            let _ = h.join();
        }
    }
}

struct ReadyTimerPayload {
    ready_tx: Option<Sender<Result<(), String>>>,
    shared: Arc<Shared>,
    expected_seq: u64,
}

unsafe extern "C" fn ready_timer_callback(_timer: CFRunLoopTimerRef, info: *mut c_void) {
    if info.is_null() {
        return;
    }
    let payload = unsafe { &mut *(info as *mut ReadyTimerPayload) };
    if let Some(tx) = payload.ready_tx.take() {
        let _ = tx.send(Ok(()));
    }
    // If stop or reinstall raced in just before CFRunLoopRun() began processing sources, stop now
    // from inside the running loop so we never deadlock.
    let should_stop = {
        let st = payload.shared.tap_state.lock();
        st.stop || st.reinstall_seq != payload.expected_seq
    };
    if should_stop {
        unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
    }
}

fn run_tap_loop(shared: Arc<Shared>, ctrl_tx: Sender<Control>, ready: Sender<Result<(), String>>) {
    let mut ready_opt = Some(ready);

    while !shared.is_stopped() {
        let ctx = Box::new(TapContext {
            tap: std::ptr::null_mut(),
            shared: shared.clone(),
            ctrl_tx: ctrl_tx.clone(),
            mods: Mutex::new(current_mod_state()),
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
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };

        unsafe {
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            if source.is_null() {
                CFMachPortInvalidate(tap);
                CFRelease(tap as *const c_void);
                drop(Box::from_raw(ctx_ptr));
                if let Some(r) = ready_opt.take() {
                    let _ = r.send(Err("CFMachPortCreateRunLoopSource returned null".to_string()));
                    return;
                }
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }

            (*ctx_ptr).tap = tap;
            let rl = CFRunLoopGetCurrent();
            CFRetain(rl as *const c_void);

            let (already_stopped, expected_seq) = {
                let mut st = shared.tap_state.lock();
                if st.stop {
                    (true, st.reinstall_seq)
                } else {
                    st.run_loop = Some(rl as usize);
                    (false, st.reinstall_seq)
                }
            };

            if already_stopped {
                CFMachPortInvalidate(tap);
                CFRelease(source as *const c_void);
                CFRelease(tap as *const c_void);
                CFRelease(rl as *const c_void);
                drop(Box::from_raw(ctx_ptr));
                break;
            }

            CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);

            // Fire a one-shot timer on the first iteration of `CFRunLoopRun()` so `ready` is
            // sent only once the run loop is actively spinning (preventing any `Drop` race).
            let is_first = ready_opt.is_some();
            let mut timer_payload = ReadyTimerPayload {
                ready_tx: ready_opt.take(),
                shared: shared.clone(),
                expected_seq,
            };
            let mut timer_ctx = CFRunLoopTimerContext {
                version: 0,
                info: (&mut timer_payload as *mut ReadyTimerPayload) as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            let timer = CFRunLoopTimerCreate(
                std::ptr::null(),
                0.0,
                0.0,
                0,
                0,
                ready_timer_callback,
                &mut timer_ctx,
            );
            if !timer.is_null() {
                CFRunLoopAddTimer(rl, timer, kCFRunLoopCommonModes);
            } else if let Some(tx) = timer_payload.ready_tx.take() {
                let _ = tx.send(Ok(()));
            }

            if is_first {
                tracing::info!("macOS Fn + Control event tap installed");
            } else {
                tracing::info!("macOS Fn + Control event tap reinstalled");
            }

            CFRunLoopRun();

            // Clear run_loop under the mutex BEFORE releasing `rl` so `stop`/`reinstall` can
            // never retain a freed `CFRunLoopRef`.
            {
                let mut st = shared.tap_state.lock();
                st.run_loop = None;
            }
            if !timer.is_null() {
                CFRunLoopTimerInvalidate(timer);
                CFRelease(timer as *const c_void);
            }
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
                machine.prune_other_keys(|vk| is_other_key_still_down(vk, s));
                let out = machine.on_key_state(s.left_ctrl, s.fn_key, now);
                emit(out);
            }
            Control::Event { ev, mods } => {
                if shared.paused.load(Ordering::Relaxed) {
                    continue;
                }
                let mut out = Vec::new();
                if ev.dir == KeyDir::Down && (ev.vk == VK_CHORD_CTRL || ev.vk == VK_CHORD_WIN) {
                    // Use the callback-time `ModState` snapshot (not a live query) so quick taps
                    // and double-taps cannot be corrupted by async dequeue latency, and prune any
                    // stuck `others` left behind by Secure Input or tap timeouts.
                    machine.prune_other_keys(|vk| is_other_key_still_down(vk, mods));
                    let (partner, partner_down) = if ev.vk == VK_CHORD_CTRL {
                        (VK_CHORD_WIN, mods.fn_key)
                    } else {
                        (VK_CHORD_CTRL, mods.left_ctrl)
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
/// and recreate the event tap after sleep wake. Returns observer tokens removed on drop.
fn register_system_notifications(shared: Arc<Shared>, ctrl_tx: Sender<Control>) -> SystemObservers {
    objc2::rc::autoreleasepool(|_| {
        use block2::RcBlock;
        use objc2_app_kit::NSWorkspace;
        use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSString};

        let tx_sleep = ctrl_tx.clone();
        let tx_wake = ctrl_tx.clone();
        let wake_shared = shared.clone();
        let tx_lock = ctrl_tx.clone();
        let tx_unlock = ctrl_tx;
        let unlock_shared = shared;

        let mut ws_tokens = Vec::with_capacity(2);
        let mut dist_tokens = Vec::with_capacity(2);

        unsafe {
            let ws_center = NSWorkspace::sharedWorkspace().notificationCenter();
            let sleep_name = NSString::from_str("NSWorkspaceWillSleepNotification");
            let wake_name = NSString::from_str("NSWorkspaceDidWakeNotification");

            let sleep_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
                let _ = tx_sleep.try_send(Control::Interrupt);
            });
            let wake_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
                if !wake_shared.is_stopped() {
                    let _ = tx_wake.try_send(Control::Resume);
                    wake_shared.request_reinstall();
                }
            });
            let obs1 = ws_center.addObserverForName_object_queue_usingBlock(Some(&sleep_name), None, None, &sleep_block);
            let obs2 = ws_center.addObserverForName_object_queue_usingBlock(Some(&wake_name), None, None, &wake_block);
            ws_tokens.push(objc2::rc::Retained::into_raw(obs1) as usize);
            ws_tokens.push(objc2::rc::Retained::into_raw(obs2) as usize);

            let dist_center = NSDistributedNotificationCenter::defaultCenter();
            let lock_name = NSString::from_str("com.apple.screenIsLocked");
            let unlock_name = NSString::from_str("com.apple.screenIsUnlocked");

            let lock_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
                let _ = tx_lock.try_send(Control::Interrupt);
            });
            let unlock_block = RcBlock::new(move |_notif: NonNull<NSNotification>| {
                if !unlock_shared.is_stopped() {
                    let _ = tx_unlock.try_send(Control::Resume);
                }
            });
            let obs3 = dist_center.addObserverForName_object_queue_usingBlock(Some(&lock_name), None, None, &lock_block);
            let obs4 = dist_center.addObserverForName_object_queue_usingBlock(Some(&unlock_name), None, None, &unlock_block);
            dist_tokens.push(objc2::rc::Retained::into_raw(obs3) as usize);
            dist_tokens.push(objc2::rc::Retained::into_raw(obs4) as usize);
        }

        SystemObservers { ws_tokens, dist_tokens }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_and_left_control_flags_drive_chord_machine() {
        let mut m = ChordMachine::new();
        let s0 = ModState::from_flags(0);
        let s1 = ModState::from_flags(MASK_SECONDARY_FN);
        let s2 = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL | MASK_LEFT_CONTROL);
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
        let s1 = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL | MASK_LEFT_CONTROL);
        let mut out = Vec::new();
        for ev in s0.diff_events(s1, 0, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.contains(&ChordOutput::HoldStart));
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
    fn releasing_right_control_while_left_control_and_fn_held_does_not_start_chord() {
        let mut m = ChordMachine::new();
        let s0 = ModState::from_flags(0);
        let s1 = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL | MASK_LEFT_CONTROL | MASK_RIGHT_CONTROL);
        let s2 = ModState::from_flags(MASK_SECONDARY_FN | MASK_CONTROL | MASK_LEFT_CONTROL);
        assert!(s1.left_ctrl && s1.right_ctrl && s1.fn_key);
        assert!(s2.left_ctrl && !s2.right_ctrl && s2.fn_key);

        let mut out = Vec::new();
        for ev in s0.diff_events(s1, 0, false) {
            out.extend(m.on_key(ev));
        }
        // Extra modifier (right_ctrl) was held, so no chord started.
        assert!(out.is_empty());
        // Releasing right_ctrl only emits MOD_RCTRL_VK Up, NOT a spurious LCtrl Down.
        for ev in s1.diff_events(s2, 100, false) {
            out.extend(m.on_key(ev));
        }
        assert!(out.is_empty(), "releasing Right Control must not start a hold: {out:?}");
    }

    #[test]
    fn extra_modifier_blocks_chord() {
        let mut m = ChordMachine::new();
        let s0 = ModState::from_flags(0);
        let s1 = ModState::from_flags(MASK_COMMAND);
        let s2 = ModState::from_flags(MASK_COMMAND | MASK_SECONDARY_FN | MASK_CONTROL | MASK_LEFT_CONTROL);
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
