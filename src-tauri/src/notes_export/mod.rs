//! Enhanced notes leaving Wren: formatted Copy (for email), Save as PDF, Save as Word, and the macOS Share
//! menu (spec `docs/superpowers/specs/2026-11-25-notes-export-design.md`).
//!
//! The page builds the content (sanitised, self-contained HTML with inline styles, clean plain text, and
//! the .docx bytes); native code only puts it on the clipboard, asks where to save, writes the file, or
//! hands it to the system. Save dialogs run here, so the page never chooses a path it can write to.

#[cfg(target_os = "macos")]
mod macos;

use tauri::AppHandle;
use tauri_plugin_dialog::DialogExt;

/// Longest HTML / plain text accepted from the page (a long meeting's notes are ~50 KB).
const MAX_TEXT_BYTES: usize = 4 * 1024 * 1024;
/// Largest .docx accepted from the page.
const MAX_DOCX_BYTES: usize = 16 * 1024 * 1024;

fn check_len(what: &str, len: usize, max: usize) -> Result<(), String> {
    if len == 0 {
        return Err(format!("Nothing to {what}."));
    }
    if len > max {
        return Err(format!("These notes are too large to {what}."));
    }
    Ok(())
}

/// A file name the save dialog can suggest: no path separators or characters Finder/Explorer refuse,
/// no leading dots, at most 120 characters, and always the given extension.
pub fn safe_file_name(suggested: &str, ext: &str) -> String {
    let mut name: String = suggested
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let lower = name.to_lowercase();
    let dotted = format!(".{ext}");
    if lower.ends_with(&dotted) {
        name.truncate(name.len() - dotted.len());
    }
    let mut name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    name = name.trim_start_matches('.').trim().to_string();
    if name.chars().count() > 120 {
        name = name.chars().take(120).collect::<String>().trim_end().to_string();
    }
    if name.is_empty() {
        name = "Meeting notes".into();
    }
    format!("{name}.{ext}")
}

/// Asks where to save. `Ok(None)` = the person cancelled.
fn ask_save_path(app: &AppHandle, suggested: &str, ext: &str, filter: &str) -> Result<Option<std::path::PathBuf>, String> {
    let picked = app
        .dialog()
        .file()
        .set_file_name(safe_file_name(suggested, ext))
        .add_filter(filter, &[ext])
        .blocking_save_file();
    let Some(picked) = picked else { return Ok(None) };
    let mut path = picked.into_path().map_err(|e| {
        tracing::warn!(error = %e, "save dialog returned a non-file path");
        "Couldn't use that location.".to_string()
    })?;
    if path.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case(ext)) != Some(true) {
        path.set_extension(ext);
    }
    Ok(Some(path))
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Copy: formatted HTML for mail apps (+ RTF on macOS) and clean plain text for everything else.
#[tauri::command(async)]
pub fn notes_copy_rich(html: String, plain: String) -> Result<(), String> {
    check_len("copy", html.len(), MAX_TEXT_BYTES)?;
    check_len("copy", plain.len(), MAX_TEXT_BYTES)?;
    copy_rich(&html, &plain).map_err(|e| {
        tracing::warn!(error = %e, "rich copy failed");
        "Couldn't copy the notes.".to_string()
    })
}

#[cfg(windows)]
fn copy_rich(html: &str, plain: &str) -> anyhow::Result<()> {
    crate::paste::clipboard::copy_html_and_text(&cf_html(html), plain)
}

#[cfg(target_os = "macos")]
fn copy_rich(html: &str, plain: &str) -> anyhow::Result<()> {
    macos::copy_rich(html, plain)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn copy_rich(_html: &str, _plain: &str) -> anyhow::Result<()> {
    anyhow::bail!("not supported on this platform")
}

/// Save as Word: the page builds the .docx; this asks where and writes it. Returns the saved file's
/// name, or `None` when cancelled.
#[tauri::command(async)]
pub fn notes_save_docx(app: AppHandle, bytes: Vec<u8>, suggested_name: String) -> Result<Option<String>, String> {
    check_len("save", bytes.len(), MAX_DOCX_BYTES)?;
    // A .docx is a zip: refuse anything that isn't, so this can't be used to write arbitrary files.
    if !bytes.starts_with(b"PK\x03\x04") {
        return Err("That isn't a Word document.".into());
    }
    let Some(path) = ask_save_path(&app, &suggested_name, "docx", "Word document")? else { return Ok(None) };
    std::fs::write(&path, &bytes).map_err(|e| {
        tracing::warn!(error = %e, path = %path.display(), "saving .docx failed");
        format!("Couldn't save {}.", display_name(&path))
    })?;
    tracing::info!(path = %path.display(), bytes = bytes.len(), "saved notes as Word");
    Ok(Some(display_name(&path)))
}

/// Save as PDF (macOS): asks where, then renders `html` off-screen into a paginated PDF there.
#[tauri::command(async)]
pub fn notes_save_pdf(app: AppHandle, html: String, suggested_name: String) -> Result<Option<String>, String> {
    check_len("save", html.len(), MAX_TEXT_BYTES)?;
    #[cfg(target_os = "macos")]
    {
        let Some(path) = ask_save_path(&app, &suggested_name, "pdf", "PDF")? else { return Ok(None) };
        macos::write_pdf(&app, &html, &path).map_err(|e| {
            tracing::warn!(error = %e, path = %path.display(), "saving PDF failed");
            format!("Couldn't save {}.", display_name(&path))
        })?;
        tracing::info!(path = %path.display(), "saved notes as PDF");
        Ok(Some(display_name(&path)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, suggested_name);
        Err("Save as PDF isn't available on this computer yet.".into())
    }
}

/// Share (macOS): the system Share menu at `rect` (CSS pixels in the main window), offering the
/// formatted notes; Mail gets `subject`.
#[tauri::command(async)]
pub fn notes_share(app: AppHandle, html: String, plain: String, subject: String, rect: [f64; 4]) -> Result<(), String> {
    check_len("share", html.len(), MAX_TEXT_BYTES)?;
    #[cfg(target_os = "macos")]
    {
        macos::share(&app, &html, &plain, &subject, rect).map_err(|e| {
            tracing::warn!(error = %e, "share failed");
            "Couldn't open the Share menu.".to_string()
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, plain, subject, rect);
        Err("Sharing isn't available on this computer yet.".into())
    }
}

/// Windows "HTML Format" clipboard payload: a header with UTF-8 byte offsets, then the document with the
/// fragment between StartFragment/EndFragment comments.
pub fn cf_html(fragment: &str) -> String {
    const HEADER_LEN: usize = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
    let prefix = "<html><head><meta charset=\"utf-8\"></head><body><!--StartFragment-->";
    let suffix = "<!--EndFragment--></body></html>";
    let start_html = HEADER_LEN;
    let start_fragment = start_html + prefix.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + suffix.len();
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{prefix}{fragment}{suffix}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_html_offsets_point_at_the_fragment() {
        let frag = "<h2>Résumé — “notes”</h2><p>✓ done</p>";
        let doc = cf_html(frag);
        let num = |key: &str| -> usize {
            let at = doc.find(key).unwrap() + key.len();
            doc[at..at + 10].parse().unwrap()
        };
        let (sh, eh, sf, ef) = (num("StartHTML:"), num("EndHTML:"), num("StartFragment:"), num("EndFragment:"));
        let bytes = doc.as_bytes();
        assert_eq!(&bytes[sf..ef], frag.as_bytes());
        assert!(doc[sh..].starts_with("<html>"));
        assert_eq!(eh, bytes.len());
        assert!(doc[sh..eh].ends_with("</html>"));
    }

    #[test]
    fn file_names_are_safe_and_keep_the_extension() {
        assert_eq!(safe_file_name("Q3 plan: review / 25 Nov", "pdf"), "Q3 plan- review - 25 Nov.pdf");
        assert_eq!(safe_file_name("notes.PDF", "pdf"), "notes.pdf");
        assert_eq!(safe_file_name("../../etc/passwd", "docx"), "-..-etc-passwd.docx");
        assert_eq!(safe_file_name("   ", "docx"), "Meeting notes.docx");
        assert_eq!(safe_file_name("a\u{0}b\nc", "pdf"), "a b c.pdf");
        let long = "x".repeat(300);
        assert_eq!(safe_file_name(&long, "pdf").len(), 124);
    }

    #[test]
    fn oversized_or_empty_content_is_refused() {
        assert!(check_len("copy", 0, 10).is_err());
        assert!(check_len("copy", 11, 10).is_err());
        assert!(check_len("copy", 10, 10).is_ok());
    }
}
