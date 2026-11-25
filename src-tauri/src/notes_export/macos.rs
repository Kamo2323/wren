//! macOS pieces of notes export: pasteboard (HTML + RTF + plain text), off-screen WKWebView → paginated
//! PDF, and the NSSharingServicePicker Share menu.
//!
//! STUB — implemented and live-tested on the MacBook (see the notes-export spec, Design §4).

use std::path::Path;

use tauri::AppHandle;

/// One pasteboard write with `public.html`, `public.rtf` (from the HTML via NSAttributedString, on the
/// main thread) and `public.utf8-plain-text`, under the paste transaction lock.
pub fn copy_rich(_html: &str, _plain: &str) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}

/// Render `html` in an off-screen WKWebView and print it to `path` as a paginated PDF without a panel.
pub fn write_pdf(_app: &AppHandle, _html: &str, _path: &Path) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}

/// Show NSSharingServicePicker anchored at `rect` (CSS px, top-left origin, main window) sharing the
/// notes as an attributed string (Mail: formatted body, `subject`).
pub fn share(_app: &AppHandle, _html: &str, _plain: &str, _subject: &str, _rect: [f64; 4]) -> anyhow::Result<()> {
    anyhow::bail!("not implemented yet")
}
