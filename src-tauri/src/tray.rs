//! System tray icon and menu: Open Wren, Pause dictation, Reinstall hotkey, Quit.
//! Pause and Reinstall are broadcast as app events; the session listens for them.

use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};

pub const MAIN_LABEL: &str = "main";
/// Payload: `bool` (true = paused).
pub const EVENT_PAUSE_TOGGLED: &str = "tray://pause-toggled";
pub const EVENT_REINSTALL_HOOK: &str = "tray://reinstall-hook";

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Wren", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause dictation", true, false, None::<&str>)?;
    let reinstall = MenuItem::with_id(app, "reinstall_hook", "Reinstall hotkey", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Wren", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &reinstall, &separator, &quit])?;

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
pub fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(MAIN_LABEL) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
    }
}
