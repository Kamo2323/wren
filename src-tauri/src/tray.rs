//! System tray icon and menu: Open Wren, Pause dictation, Reinstall hotkey, Quit.
//! Pause and Reinstall are broadcast as app events; the session listens for them.

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::updater::{EVENT_OPEN_UPDATES, UpdateStatus};

pub const MAIN_LABEL: &str = "main";
/// Payload: `bool` (true = paused).
pub const EVENT_PAUSE_TOGGLED: &str = "tray://pause-toggled";
pub const EVENT_REINSTALL_HOOK: &str = "tray://reinstall-hook";

static UPDATE_MENU_ITEM: Mutex<Option<MenuItem<Wry>>> = Mutex::new(None);

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Wren", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause dictation", true, false, None::<&str>)?;
    let reinstall = MenuItem::with_id(app, "reinstall_hook", "Reinstall hotkey", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Wren", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &reinstall, &update, &separator, &quit])?;
    *UPDATE_MENU_ITEM.lock() = Some(update);

    let pause_item = pause.clone();
    let mut builder = TrayIconBuilder::with_id("wren")
        .tooltip("Wren")
        .menu(&menu)
        .show_menu_on_left_click(false)
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
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
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

/// Show the main window the first time, once. Later calls (and calls when Wren started hidden) do
/// nothing, so the page can call it freely.
pub fn first_show(app: &AppHandle) {
    if FIRST_SHOW_PENDING.swap(false, Ordering::SeqCst)
        && let Some(win) = app.get_webview_window(MAIN_LABEL)
    {
        let _ = win.show();
    }
}

pub fn show_main(app: &AppHandle) {
    FIRST_SHOW_PENDING.store(false, Ordering::SeqCst);
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
