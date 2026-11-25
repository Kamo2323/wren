//! Ask before quitting mid-meeting on macOS (spec §5.5). Cmd+Q, the app menu's Quit and Dock → Quit
//! all call `-[NSApplication terminate:]`, and tao's app delegate only implements
//! `applicationWillTerminate:`, so Tauri goes straight to `RunEvent::Exit`. `ExitRequested`, where
//! the "Stop meeting notes and quit?" question lives, never fires, and the meeting just ended.
//!
//! [`install`] adds `applicationShouldTerminate:` to tao's delegate class. While a meeting records
//! and the quit wasn't confirmed, it answers "cancel" and asks instead. Logout, restart and shutdown
//! (a quit Apple Event carrying a reason) are let through; the `Exit` handler still ends the meeting
//! properly.

use objc2::ffi::{class_addMethod, object_getClass};
use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{class, msg_send, sel};
use std::sync::OnceLock;
use tauri::AppHandle;

/// `NSApplicationTerminateReply`.
const TERMINATE_CANCEL: usize = 0;
const TERMINATE_NOW: usize = 1;
/// `keyAEQuitReason` ('why?'): present on the quit Apple Event sent for logout / restart / shutdown.
const KEY_QUIT_REASON: u32 = u32::from_be_bytes(*b"why?");

static APP: OnceLock<AppHandle> = OnceLock::new();

/// What to answer `applicationShouldTerminate:`; `true` means cancel and ask.
pub fn should_ask(recording: bool, confirmed: bool, system_quit: bool) -> bool {
    recording && !confirmed && !system_quit
}

/// Whether the current Apple Event is the system quitting us (logout, restart, shutdown).
fn system_quit() -> bool {
    unsafe {
        let mgr: *mut AnyObject = msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        if mgr.is_null() {
            return false;
        }
        let event: *mut AnyObject = msg_send![mgr, currentAppleEvent];
        if event.is_null() {
            return false;
        }
        let why: *mut AnyObject = msg_send![event, attributeDescriptorForKeyword: KEY_QUIT_REASON];
        !why.is_null()
    }
}

/// `-[TaoAppDelegate applicationShouldTerminate:]`. `extern "C"`: never unwinds into AppKit.
unsafe extern "C" fn should_terminate(_this: *mut AnyObject, _sel: Sel, _sender: *mut AnyObject) -> usize {
    let Some(app) = APP.get() else { return TERMINATE_NOW };
    let recording = super::commands::recording(app);
    let confirmed = super::commands::QUIT_CONFIRMED.load(std::sync::atomic::Ordering::SeqCst);
    if !should_ask(recording, confirmed, recording && !confirmed && system_quit()) {
        return TERMINATE_NOW;
    }
    tracing::info!("quit asked while a meeting records; asking first");
    super::commands::ask_before_quit(app);
    TERMINATE_CANCEL
}

/// Install the hook. Call on the main thread once the app delegate exists (`RunEvent::Ready`).
pub fn install(app: &AppHandle) {
    if APP.set(app.clone()).is_err() {
        return;
    }
    unsafe {
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let delegate: *mut AnyObject = if ns_app.is_null() { std::ptr::null_mut() } else { msg_send![ns_app, delegate] };
        if delegate.is_null() {
            tracing::warn!("no app delegate; Cmd+Q won't ask before ending a meeting");
            return;
        }
        let cls = object_getClass(delegate) as *mut AnyClass;
        type ShouldTerminate = unsafe extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> usize;
        // Every Objective-C method IMP is called through objc_msgSend with its real signature; `Imp`
        // is only the opaque pointer type the runtime stores.
        let imp = std::mem::transmute::<ShouldTerminate, Imp>(should_terminate as ShouldTerminate);
        let added = class_addMethod(cls, sel!(applicationShouldTerminate:), imp, c"Q@:@".as_ptr());
        if added.as_bool() {
            tracing::info!("quit confirmation hook installed");
        } else {
            tracing::warn!("app delegate already answers applicationShouldTerminate:; quit hook not installed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_only_while_recording_unconfirmed_and_not_logging_out() {
        assert!(should_ask(true, false, false));
        assert!(!should_ask(false, false, false), "no meeting: quit at once");
        assert!(!should_ask(true, true, false), "confirmed Stop and quit");
        assert!(!should_ask(true, false, true), "logout / restart / shutdown aren't blocked");
    }

    #[test]
    fn quit_reason_keyword_is_why() {
        assert_eq!(KEY_QUIT_REASON, 0x7768_793F);
    }
}
