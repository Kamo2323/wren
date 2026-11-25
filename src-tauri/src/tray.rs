//! System tray icon and menu: Open Wren, Pause dictation, Reinstall hotkey, Quit.
//! Pause and Reinstall are broadcast as app events; the session listens for them.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
#[cfg(not(target_os = "macos"))]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::meetings::MeetingStatus;
use crate::updater::{EVENT_OPEN_UPDATES, UpdateStatus};

pub const MAIN_LABEL: &str = "main";
/// Payload: `bool` (true = paused).
pub const EVENT_PAUSE_TOGGLED: &str = "tray://pause-toggled";
pub const EVENT_REINSTALL_HOOK: &str = "tray://reinstall-hook";

static UPDATE_MENU_ITEM: Mutex<Option<MenuItem<Wry>>> = Mutex::new(None);
/// The meeting items (pause/resume, stop), enabled only while a meeting records.
static MEETING_ITEMS: Mutex<Option<(MenuItem<Wry>, MenuItem<Wry>)>> = Mutex::new(None);
/// Last meeting state shown, so repeated status events don't redraw the icon.
static MEETING_SHOWN: Mutex<Option<Option<MeetingStatus>>> = Mutex::new(None);

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Wren", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause dictation", true, false, None::<&str>)?;
    let reinstall = MenuItem::with_id(app, "reinstall_hook", "Reinstall hotkey", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let meeting_pause = MenuItem::with_id(app, "meeting_pause", "Pause meeting notes", false, None::<&str>)?;
    let meeting_stop = MenuItem::with_id(app, "meeting_stop", "Stop meeting notes", false, None::<&str>)?;
    let separator2 = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Wren", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[&open, &meeting_pause, &meeting_stop, &separator2, &pause, &reinstall, &update, &separator, &quit],
    )?;
    *UPDATE_MENU_ITEM.lock() = Some(update);
    *MEETING_ITEMS.lock() = Some((meeting_pause, meeting_stop));

    let pause_item = pause.clone();
    let mut builder = TrayIconBuilder::with_id("wren")
        .tooltip("Wren")
        .menu(&menu)
        // On macOS, clicking the menu bar icon opens the menu; on Windows left-click opens Wren.
        .show_menu_on_left_click(cfg!(target_os = "macos"))
        .on_menu_event(move |app, event| match event.id().as_ref() {
            "open" => show_main(app),
            "pause" => {
                let paused = pause_item.is_checked().unwrap_or(false);
                let _ = app.emit(EVENT_PAUSE_TOGGLED, paused);
            }
            "reinstall_hook" => {
                let _ = app.emit(EVENT_REINSTALL_HOOK, ());
            }
            "update" => {
                show_main(app);
                let _ = app.emit(EVENT_OPEN_UPDATES, ());
            }
            "meeting_pause" | "meeting_stop" => crate::meetings::commands::tray_action(app, event.id().as_ref()),
            "quit" => {
                // A meeting is recording: ask in the main window first (spec §5.5).
                let recording = app
                    .try_state::<crate::meetings::commands::Meetings>()
                    .is_some_and(|m| m.0.active().is_some());
                if recording {
                    show_main(app);
                    let _ = app.emit(crate::meetings::commands::EVENT_CONFIRM_QUIT, ());
                } else {
                    app.exit(0);
                }
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            #[cfg(not(target_os = "macos"))]
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
            #[cfg(target_os = "macos")]
            let _ = (tray, event);
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

/// Bring the main window back (from the tray, a second launch, or a popup click).
/// The main window is still waiting for its first show (the page says when it has painted).
static FIRST_SHOW_PENDING: AtomicBool = AtomicBool::new(false);

/// Keep the main window hidden until the page has painted; `first_show` (from the page, or the
/// fallback timer) then shows it. Call once from `setup`, unless Wren starts hidden.
pub fn arm_first_show() {
    FIRST_SHOW_PENDING.store(true, Ordering::SeqCst);
}

#[cfg(target_os = "macos")]
fn activate_macos_app(app: &AppHandle) {
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    let _ = app.run_on_main_thread(|| {
        if let Some(mtm) = objc2_foundation::MainThreadMarker::new() {
            let ns_app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            #[allow(deprecated)]
            ns_app.activateIgnoringOtherApps(true);
        }
    });
}

/// Show the main window the first time, once. Later calls (and calls when Wren started hidden) do
/// nothing, so the page can call it freely.
pub fn first_show(app: &AppHandle) {
    if FIRST_SHOW_PENDING.swap(false, Ordering::SeqCst)
        && let Some(win) = app.get_webview_window(MAIN_LABEL)
    {
        #[cfg(target_os = "macos")]
        activate_macos_app(app);
        let _ = win.show();
    }
}

pub fn show_main(app: &AppHandle) {
    FIRST_SHOW_PENDING.store(false, Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    activate_macos_app(app);
    if let Some(win) = app.get_webview_window(MAIN_LABEL) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}

/// Keeps the tray menu's update item in sync with [`UpdateStatus`].
pub fn set_update_status(status: &UpdateStatus) {
    let guard = UPDATE_MENU_ITEM.lock();
    let Some(item) = guard.as_ref() else { return };
    let label = match status {
        UpdateStatus::Available { version, .. } => format!("Update available (v{version})"),
        UpdateStatus::Downloading { percent: Some(p), .. } => format!("Downloading update ({p}%)…"),
        UpdateStatus::Downloading { percent: None, .. } => "Downloading update…".to_string(),
        UpdateStatus::Installing { .. } => "Installing update…".to_string(),
        UpdateStatus::Checking => "Checking for updates…".to_string(),
        _ => "Check for updates".to_string(),
    };
    let _ = item.set_text(label);
}

/// Show the meeting state in the tray: a red dot on the icon, a tooltip, and the pause/stop items.
pub fn set_meeting_state(app: &AppHandle, status: Option<MeetingStatus>) {
    {
        let mut shown = MEETING_SHOWN.lock();
        if *shown == Some(status) {
            return;
        }
        *shown = Some(status);
    }
    if let Some((pause, stop)) = MEETING_ITEMS.lock().as_ref() {
        let _ = pause.set_enabled(status.is_some());
        let _ = stop.set_enabled(status.is_some());
        let _ = pause.set_text(if status == Some(MeetingStatus::Paused) { "Resume meeting notes" } else { "Pause meeting notes" });
    }
    let Some(tray) = app.tray_by_id("wren") else { return };
    let tooltip = match status {
        Some(MeetingStatus::Paused) => "Wren — meeting notes paused",
        Some(_) => "Wren — recording meeting notes",
        None => "Wren",
    };
    let _ = tray.set_tooltip(Some(tooltip));
    if let Some(icon) = app.default_window_icon() {
        let img = if status == Some(MeetingStatus::Recording) {
            let (w, h) = (icon.width(), icon.height());
            let mut rgba = icon.rgba().to_vec();
            crate::meetings::commands::recording_dot(&mut rgba, w, h);
            tauri::image::Image::new_owned(rgba, w, h)
        } else {
            icon.clone().to_owned()
        };
        let _ = tray.set_icon(Some(img));
    }
}
