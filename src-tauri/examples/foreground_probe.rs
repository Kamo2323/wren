//! Live check of app detection for Claude clean-up: waits (default 2 s) so you can bring another app to
//! the front, then calls the same `paste::foreground()` the dictation session uses when recording stops
//! and prints the app name, window title and the `ai_cleanup::category()` it maps to.
//!
//!   cargo run --example foreground_probe            # 2 s delay
//!   cargo run --example foreground_probe -- 5       # 5 s delay
//!
//! The title is printed here only because this is a local proof tool; the app never logs it.

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    std::thread::sleep(std::time::Duration::from_secs(secs));
    #[cfg(target_os = "macos")]
    println!("accessibility_trusted={}", wren_lib::hotkey::accessibility_trusted(false));
    let fg = wren_lib::paste::foreground();
    let cat = wren_lib::ai_cleanup::category(&fg.process_name, &fg.window_title);
    // The raw AX read, to tell a real title from the app-name fallback.
    #[cfg(target_os = "macos")]
    println!("ax_title={:?}", wren_lib::watcher::ax_macos::focused_window_token_and_title(fg.pid).1);
    println!("pid={}", fg.pid);
    println!("name={}", fg.process_name);
    println!("title={}", fg.window_title);
    println!("category={}", cat.as_str());
}
