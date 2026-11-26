//! Windows pieces of notes export: a hidden WebView2 window → paginated PDF (`ICoreWebView2_7::PrintToPdf`),
//! and the Email button's draft (a `.eml` opened by the default mail app). Notes-export spec + the Windows
//! brief `docs/superpowers/plans/2026-11-26-windows-notes-export.md`.
//!
//! PDF threading: the command runs on a Tauri worker thread. The hidden window is built from there (Tauri
//! creates it on the main thread); its page-load callback and the WebView2 print callback run on the main
//! thread and report back over a channel; the worker waits with timeouts. The main thread never waits on
//! the worker. One PDF at a time ([`PdfSlot`]), held until the print has really finished: a print that
//! outlives the worker's timeout keeps its window and its slot until WebView2 reports back (or, if it
//! never does, until a long grace period ends), so a running print is never torn down under WebView2.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, SystemTime};

use anyhow::{Context, anyhow, bail};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

const PDF_LOAD_TIMEOUT: Duration = Duration::from_secs(15);
const PDF_PRINT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a print that missed its timeout may keep running before its window is closed anyway.
const HUNG_PRINT_GRACE: Duration = Duration::from_secs(90);
/// Page margins: 18 mm in inches (WebView2 print settings are in inches).
const MARGIN_IN: f64 = 18.0 / 25.4;
const A4_IN: (f64, f64) = (210.0 / 25.4, 297.0 / 25.4);
const LETTER_IN: (f64, f64) = (8.5, 11.0);
/// Email drafts older than this are deleted when the next one is made (mail apps read them on open).
const DRAFT_MAX_AGE: Duration = Duration::from_secs(60 * 60);
const OPEN_TIMEOUT: Duration = Duration::from_secs(20);

pub const BUSY_MESSAGE: &str = "Another PDF is still being saved. Try again in a moment.";

/// Paper for the user's region: Letter in the US and Canada, A4 everywhere else (same rule as macOS).
pub(crate) fn paper_for_region(region: Option<&str>) -> (f64, f64) {
    match region.map(|r| r.to_ascii_uppercase()) {
        Some(r) if r == "US" || r == "CA" => LETTER_IN,
        _ => A4_IN,
    }
}

/// The two-letter region of the user's regional format (Settings → Time & language → Region).
fn current_region() -> Option<String> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetLocaleInfoEx(locale: *const u16, lctype: u32, data: *mut u16, cch: i32) -> i32;
    }
    const LOCALE_SISO3166CTRYNAME: u32 = 0x5A;
    let mut buf = [0u16; 16];
    // SAFETY: null = LOCALE_NAME_USER_DEFAULT; the buffer length is passed.
    let n = unsafe { GetLocaleInfoEx(std::ptr::null(), LOCALE_SISO3166CTRYNAME, buf.as_mut_ptr(), buf.len() as i32) };
    (n > 1).then(|| String::from_utf16_lossy(&buf[..n as usize - 1]))
}

// ---- one PDF at a time ------------------------------------------------------------------------------

static PDF_BUSY: AtomicBool = AtomicBool::new(false);

/// The right to make a PDF. Taken before the save dialog; released when dropped, which happens only once
/// the job's print has finished (or was never started).
pub struct PdfSlot(());

impl PdfSlot {
    pub fn take() -> Option<Self> {
        PDF_BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).ok().map(|_| PdfSlot(()))
    }
}

impl Drop for PdfSlot {
    fn drop(&mut self) {
        PDF_BUSY.store(false, Ordering::Release);
    }
}

enum PdfEvent {
    Loaded,
    Done(Result<(), String>),
}

struct PdfJob {
    id: u64,
    /// The worker's channel; `None` once the worker stopped listening.
    events: Option<mpsc::Sender<PdfEvent>>,
    printing: bool,
    /// Filled in once `build()` returns (callbacks get the window as an argument).
    window: Option<WebviewWindow>,
    html_file: PathBuf,
    target: PathBuf,
    paper: (f64, f64),
    _slot: PdfSlot,
}

impl PdfJob {
    fn send(&self, event: PdfEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event);
        }
    }
}

/// The live job, if any (only one can exist: it owns the [`PdfSlot`]).
static JOB: Mutex<Option<PdfJob>> = Mutex::new(None);
static NEXT_PDF_JOB: AtomicU64 = AtomicU64::new(1);

fn job_lock() -> std::sync::MutexGuard<'static, Option<PdfJob>> {
    JOB.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes job `id` (if it is still the live one) and returns it, without holding the lock afterwards.
fn take_job(id: u64) -> Option<PdfJob> {
    let mut slot = job_lock();
    if slot.as_ref().is_some_and(|j| j.id == id) { slot.take() } else { None }
}

/// Closes the hidden window (queued on the main thread, after the current callback returns), deletes the
/// temporary page, and releases the slot (by dropping the job).
fn tear_down(job: PdfJob) {
    if let Some(window) = job.window.clone() {
        let w = window.clone();
        if let Err(e) = window.run_on_main_thread(move || {
            let _ = w.destroy();
        }) {
            tracing::warn!(error = %e, "couldn't close the PDF window");
        }
    }
    let _ = std::fs::remove_file(&job.html_file);
}

/// Reports the result once and tears the job down. Called from WebView2 callbacks (main thread).
fn finish(id: u64, result: Result<(), String>) {
    if let Some(job) = take_job(id) {
        job.send(PdfEvent::Done(result));
        tear_down(job);
    }
}

/// The page finished loading: start printing (once).
fn on_loaded(window: &WebviewWindow, id: u64) {
    let (target, paper) = {
        let mut guard = job_lock();
        let Some(job) = guard.as_mut().filter(|j| j.id == id) else { return };
        if job.printing || job.events.is_none() {
            return;
        }
        job.printing = true;
        job.send(PdfEvent::Loaded);
        (job.target.clone(), job.paper)
    };
    let started = window.with_webview(move |wv| {
        // SAFETY: WebView2 calls on the main thread, where `with_webview` runs this closure.
        if let Err(e) = unsafe { print_to_pdf(wv.controller(), &target, paper, id) } {
            finish(id, Err(format!("starting the print failed: {e}")));
        }
    });
    if let Err(e) = started {
        finish(id, Err(format!("couldn't reach the PDF window's WebView2: {e}")));
    }
}

unsafe fn print_to_pdf(
    controller: webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Controller,
    target: &Path,
    (width, height): (f64, f64),
    id: u64,
) -> windows_core::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT, ICoreWebView2_2, ICoreWebView2_7, ICoreWebView2Environment6,
    };
    use webview2_com::PrintToPdfCompletedHandler;
    use windows_core::{HSTRING, Interface};
    unsafe {
        let core = controller.CoreWebView2()?;
        let env: ICoreWebView2Environment6 = core.cast::<ICoreWebView2_2>()?.Environment()?.cast()?;
        let settings = env.CreatePrintSettings()?;
        settings.SetOrientation(COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT)?;
        settings.SetPageWidth(width)?;
        settings.SetPageHeight(height)?;
        settings.SetMarginTop(MARGIN_IN)?;
        settings.SetMarginBottom(MARGIN_IN)?;
        settings.SetMarginLeft(MARGIN_IN)?;
        settings.SetMarginRight(MARGIN_IN)?;
        settings.SetScaleFactor(1.0)?;
        settings.SetShouldPrintBackgrounds(true)?;
        settings.SetShouldPrintHeaderAndFooter(false)?;
        settings.SetShouldPrintSelectionOnly(false)?;
        let handler = PrintToPdfCompletedHandler::create(Box::new(move |hr, ok| {
            let result = match hr {
                Err(e) => Err(format!("the print failed: {e}")),
                Ok(()) if !ok => Err("the print didn't succeed".to_string()),
                Ok(()) => Ok(()),
            };
            finish(id, result);
            Ok(())
        }));
        core.cast::<ICoreWebView2_7>()?.PrintToPdf(&HSTRING::from(target.as_os_str()), &settings, &handler)
    }
}

/// The worker stopped waiting. Before printing starts, drop everything now. While WebView2 is printing,
/// leave it: stop reporting, and let the print callback tear down — or, if it never comes, close the
/// window after [`HUNG_PRINT_GRACE`].
fn abandon(id: u64) {
    let printing = {
        let mut guard = job_lock();
        let Some(job) = guard.as_mut().filter(|j| j.id == id) else { return };
        job.events = None;
        job.printing
    };
    if !printing {
        if let Some(job) = take_job(id) {
            tear_down(job);
        }
        return;
    }
    let spawned = std::thread::Builder::new().name("wren-pdf-reaper".into()).spawn(move || {
        std::thread::sleep(HUNG_PRINT_GRACE);
        if let Some(job) = take_job(id) {
            tracing::warn!(id, "a PDF print never finished; closing its window");
            tear_down(job);
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "couldn't start the PDF reaper");
    }
}

fn verify_pdf(path: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("no PDF at {}", path.display()))?;
    if !bytes.starts_with(b"%PDF-") {
        bail!("{} isn't a PDF ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

fn temp_dir(kind: &str) -> anyhow::Result<PathBuf> {
    let dir = std::env::temp_dir().join("Wren").join(kind);
    std::fs::create_dir_all(&dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    Ok(dir)
}

/// Renders `html` (a complete document) in a hidden WebView2 window and prints it to `path` as a
/// paginated PDF. Blocks the calling worker thread; never call it on the main thread.
pub fn write_pdf(app: &AppHandle, slot: PdfSlot, html: &str, path: &Path) -> anyhow::Result<()> {
    // The save dialog already confirmed replacing it; removing it first means a stale file can't pass.
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("couldn't replace {}", path.display()));
        }
        _ => {}
    }
    let id = NEXT_PDF_JOB.fetch_add(1, Ordering::Relaxed);
    // The page goes in a temporary file (NavigateToString caps pages at 2 MB); left-overs from a crash
    // are swept here, which is safe because this job holds the only slot.
    let dir = temp_dir("pdf")?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let html_file = dir.join(format!("notes-{id}.html"));
    std::fs::write(&html_file, html).with_context(|| format!("couldn't write {}", html_file.display()))?;
    let url = tauri::Url::from_file_path(&html_file).map_err(|_| anyhow!("bad temp path {}", html_file.display()))?;

    let (tx, rx) = mpsc::channel();
    *job_lock() = Some(PdfJob {
        id,
        events: Some(tx),
        printing: false,
        window: None,
        html_file: html_file.clone(),
        target: path.to_owned(),
        paper: paper_for_region(current_region().as_deref()),
        _slot: slot,
    });

    let built = WebviewWindowBuilder::new(app, format!("notes-pdf-{id}"), WebviewUrl::External(url))
        .title("Wren PDF")
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .decorations(false)
        .inner_size(800.0, 1100.0)
        .on_page_load(move |window, payload| {
            if payload.event() == PageLoadEvent::Finished && payload.url().scheme() == "file" {
                on_loaded(&window, id);
            }
        })
        .build();
    match built {
        Ok(window) => {
            let mut guard = job_lock();
            match guard.as_mut().filter(|j| j.id == id) {
                Some(job) => job.window = Some(window),
                // Already finished (failed fast): close the window ourselves.
                None => {
                    drop(guard);
                    let _ = window.destroy();
                }
            }
        }
        Err(e) => {
            if let Some(job) = take_job(id) {
                tear_down(job);
            }
            bail!("couldn't create the PDF window: {e}");
        }
    }

    match rx.recv_timeout(PDF_LOAD_TIMEOUT) {
        Ok(PdfEvent::Loaded) => {}
        Ok(PdfEvent::Done(r)) => {
            r.map_err(|e| anyhow!(e))?;
            bail!("the PDF job ended before the notes loaded");
        }
        Err(_) => {
            abandon(id);
            bail!("the notes didn't load within {PDF_LOAD_TIMEOUT:?}");
        }
    }
    match rx.recv_timeout(PDF_PRINT_TIMEOUT) {
        Ok(PdfEvent::Done(r)) => r.map_err(|e| anyhow!(e))?,
        Ok(PdfEvent::Loaded) => bail!("unexpected second load event"),
        Err(_) => {
            abandon(id);
            bail!("printing to PDF didn't finish within {PDF_PRINT_TIMEOUT:?}");
        }
    }
    verify_pdf(path)
}

// ---- Email draft ------------------------------------------------------------------------------------

/// Why the draft couldn't be opened, in words for the person.
#[derive(Debug)]
pub enum EmailError {
    NoMailApp,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for EmailError {
    fn from(e: anyhow::Error) -> Self {
        EmailError::Other(e)
    }
}

/// The app registered to open `.eml` files, if any (its display name).
fn eml_handler() -> Option<String> {
    use windows::Win32::UI::Shell::{ASSOCF_INIT_IGNOREUNKNOWN, ASSOCF_NOTRUNCATE, ASSOCSTR_FRIENDLYAPPNAME, AssocQueryStringW};
    use windows::core::{PCWSTR, PWSTR, w};
    let mut buf = [0u16; 260];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer and its length are passed together.
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_INIT_IGNOREUNKNOWN | ASSOCF_NOTRUNCATE,
            ASSOCSTR_FRIENDLYAPPNAME,
            w!(".eml"),
            PCWSTR::null(),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
    };
    if hr.is_err() {
        return None;
    }
    let name = String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1).min(buf.len())]);
    Some(name.trim_end_matches('\0').to_string())
}

/// Deletes draft folders (and stray files) last changed more than `max_age` ago.
fn sweep_old_drafts(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        if old {
            let p = e.path();
            let _ = if p.is_dir() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
        }
    }
}

/// Writes the notes as an unsent `.eml` draft and opens it in the default mail app. Returns the app's name.
pub fn email_draft(app: &AppHandle, html: &str, plain: &str, subject: &str) -> Result<String, EmailError> {
    let Some(handler) = eml_handler() else { return Err(EmailError::NoMailApp) };
    let root = temp_dir("email-drafts")?;
    sweep_old_drafts(&root, DRAFT_MAX_AGE);
    // One folder per draft, so the file keeps a readable name and never collides with one still open.
    let seed = uuid::Uuid::new_v4().simple().to_string();
    let dir = root.join(&seed[..12]);
    std::fs::create_dir_all(&dir).with_context(|| format!("couldn't create {}", dir.display()))?;
    let subject = super::eml::clean_subject(subject);
    let file = dir.join(super::safe_file_name(&subject, "eml"));
    let date = chrono::Local::now().to_rfc2822();
    std::fs::write(&file, super::eml::build_eml(&subject, html, plain, &date, &seed))
        .with_context(|| format!("couldn't write {}", file.display()))?;

    // ShellExecute may hand off to shell extensions, which want an STA thread: the main thread is one.
    let (tx, rx) = mpsc::channel();
    let target = file.clone();
    app.run_on_main_thread(move || {
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        use windows::core::{HSTRING, PCWSTR, w};
        // SAFETY: plain ShellExecute on the main (STA) thread with valid strings.
        let code = unsafe {
            ShellExecuteW(None, w!("open"), &HSTRING::from(target.as_os_str()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
        };
        let _ = tx.send(code.0 as isize);
    })
    .map_err(|e| anyhow!("couldn't reach the main thread: {e}"))?;
    let code = rx.recv_timeout(OPEN_TIMEOUT).map_err(|_| anyhow!("opening the draft didn't answer within {OPEN_TIMEOUT:?}"))?;
    // ShellExecute returns a value > 32 on success; 31 = SE_ERR_NOASSOC.
    match code {
        c if c > 32 => {
            tracing::info!(path = %file.display(), app = %handler, "opened notes as an email draft");
            Ok(handler)
        }
        31 => Err(EmailError::NoMailApp),
        c => Err(EmailError::Other(anyhow!("ShellExecute failed with code {c} for {}", file.display()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_is_letter_only_in_us_and_canada() {
        assert_eq!(paper_for_region(Some("US")), LETTER_IN);
        assert_eq!(paper_for_region(Some("ca")), LETTER_IN);
        assert_eq!(paper_for_region(Some("GB")), A4_IN);
        assert_eq!(paper_for_region(None), A4_IN);
        assert!((A4_IN.0 - 8.2677).abs() < 0.001 && (A4_IN.1 - 11.6929).abs() < 0.001);
        assert!((MARGIN_IN - 0.70866).abs() < 0.0001);
    }

    #[test]
    fn region_is_read_from_windows() {
        let r = current_region().expect("a region code");
        assert_eq!(r.len(), 2, "{r}");
        assert!(r.chars().all(|c| c.is_ascii_uppercase()), "{r}");
    }

    #[test]
    fn only_one_pdf_slot_at_a_time() {
        // Other tests don't take slots, so this one owns the flag.
        let first = PdfSlot::take().expect("first slot");
        assert!(PdfSlot::take().is_none(), "second is refused while the first is held");
        drop(first);
        let again = PdfSlot::take().expect("free again once released");
        drop(again);
    }

    #[test]
    fn old_drafts_are_swept_and_new_ones_kept() {
        let root = std::env::temp_dir().join(format!("wren-sweep-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("draft")).unwrap();
        std::fs::write(root.join("draft").join("a.eml"), "x").unwrap();
        std::fs::write(root.join("stray.eml"), "x").unwrap();
        sweep_old_drafts(&root, DRAFT_MAX_AGE);
        assert!(root.join("draft").join("a.eml").exists() && root.join("stray.eml").exists(), "fresh drafts stay");
        std::thread::sleep(Duration::from_millis(50));
        sweep_old_drafts(&root, Duration::from_millis(10));
        assert!(!root.join("draft").exists() && !root.join("stray.eml").exists(), "old drafts go");
        let _ = std::fs::remove_dir_all(&root);
    }
}
