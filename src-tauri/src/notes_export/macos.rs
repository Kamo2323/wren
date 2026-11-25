//! macOS pieces of notes export: pasteboard (HTML + RTF + plain text), off-screen WKWebView → paginated
//! PDF, and the NSSharingServicePicker Share menu (notes-export spec, Design §4).
//!
//! Threading: the commands run on Tauri worker threads. AppKit's HTML importer, WebKit and the share
//! picker are main-thread only, so that work is sent to the main queue and the worker waits (with a
//! timeout) for the answer. The main thread never waits on the worker, so nothing here can deadlock it.
//!
//! Live proof: `cargo run --example notes_export_proof` (and the ignored test at the bottom).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use crossbeam_channel::{Sender, bounded};
use dispatch2::DispatchQueue;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAttributedStringDocumentFormats, NSBackingStoreType, NSCharacterEncodingDocumentOption, NSDocumentTypeDocumentAttribute, NSDocumentTypeDocumentOption,
    NSHTMLTextDocumentType, NSPasteboard, NSPasteboardItem, NSPasteboardWriting, NSPrintInfo, NSPrintJobSavingURL,
    NSPrintOperation, NSPrintSaveJob, NSPrintingPaginationMode, NSRTFTextDocumentType, NSSharingService,
    NSSharingServiceNameComposeEmail, NSSharingServicePicker, NSSharingServicePickerDelegate, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSAttributedString, NSData, NSDictionary, NSError, NSLocale, NSNumber, NSObject, NSObjectProtocol, NSPoint,
    NSRange, NSRect, NSRectEdge, NSSize, NSString, NSURL,
};
use objc2_web_kit::{WKNavigation, WKNavigationDelegate, WKWebView, WKWebViewConfiguration};
use tauri::{AppHandle, Manager};

/// `NSUTF8StringEncoding`.
const UTF8_ENCODING: usize = 4;
/// HTML → attributed string on the main thread (WebKit-backed; first use can take ~1 s).
const HTML_IMPORT_TIMEOUT: Duration = Duration::from_secs(10);
const PDF_LOAD_TIMEOUT: Duration = Duration::from_secs(15);
const PDF_PRINT_TIMEOUT: Duration = Duration::from_secs(30);
const SHARE_TIMEOUT: Duration = Duration::from_secs(10);
/// Page margins: 18 mm in points.
const MARGIN_PT: f64 = 18.0 / 25.4 * 72.0;
const A4: (f64, f64) = (595.276, 841.89);
const LETTER: (f64, f64) = (612.0, 792.0);

/// The page sends an HTML fragment; pasteboard readers and the HTML importer want a document.
pub(crate) fn wrap_html(fragment: &str) -> String {
    format!("<html><head><meta charset=\"utf-8\"></head><body>{fragment}</body></html>")
}

/// Runs `f` on the main thread and waits up to `timeout` for its result. Runs inline when already on
/// the main thread. If the wait times out, `f` may still run later; its result is then dropped.
fn on_main<R: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce(MainThreadMarker) -> R + Send + 'static,
) -> anyhow::Result<R> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Ok(autoreleasepool(|_| f(mtm)));
    }
    let (tx, rx) = bounded(1);
    DispatchQueue::main().exec_async(move || {
        // SAFETY: blocks on the main queue run on the main thread.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let _ = tx.send(autoreleasepool(|_| f(mtm)));
    });
    rx.recv_timeout(timeout).map_err(|_| anyhow!("the main thread didn't answer within {timeout:?}"))
}

/// AppKit's HTML import (main thread only: it spins up WebKit).
fn html_to_attributed(_mtm: MainThreadMarker, wrapped_html: &str) -> anyhow::Result<Retained<NSAttributedString>> {
    let data = NSData::with_bytes(wrapped_html.as_bytes());
    let encoding = NSNumber::new_usize(UTF8_ENCODING);
    // SAFETY: AppKit's exported constant strings.
    let (doc_type_key, encoding_key, html_type) =
        unsafe { (NSDocumentTypeDocumentOption, NSCharacterEncodingDocumentOption, NSHTMLTextDocumentType) };
    let values: [&AnyObject; 2] = [html_type.as_ref(), encoding.as_ref()];
    let options = NSDictionary::from_slices(&[doc_type_key, encoding_key], &values);
    // SAFETY: options use the documented keys and value types.
    unsafe {
        NSAttributedString::initWithData_options_documentAttributes_error(
            NSAttributedString::alloc(),
            &data,
            &options,
            None,
        )
    }
    .map_err(|e| anyhow!("HTML import failed: {}", e.localizedDescription()))
}

fn attributed_to_rtf(attributed: &NSAttributedString) -> anyhow::Result<Vec<u8>> {
    // SAFETY: AppKit's exported constant strings.
    let (doc_type_key, rtf_type) = unsafe { (NSDocumentTypeDocumentAttribute, NSRTFTextDocumentType) };
    let values: [&AnyObject; 1] = [rtf_type.as_ref()];
    let attrs = NSDictionary::from_slices(&[doc_type_key], &values);
    let range = NSRange::new(0, attributed.length());
    // SAFETY: the range covers the string; attrs use the documented key and value type.
    let data = unsafe { attributed.dataFromRange_documentAttributes_error(range, &attrs) }
        .map_err(|e| anyhow!("RTF export failed: {}", e.localizedDescription()))?;
    Ok(data.to_vec())
}

/// Wren's pasteboard lock: held for the write so it can't interleave with a dictation paste.
fn pasteboard_lock() -> impl Drop {
    crate::paste::clipboard_macos::transaction_lock()
}

/// One pasteboard write with `public.html`, `public.rtf` (from the HTML via NSAttributedString, on the
/// main thread) and `public.utf8-plain-text`, under the paste transaction lock.
pub fn copy_rich(html: &str, plain: &str) -> anyhow::Result<()> {
    let wrapped = wrap_html(html);
    // Convert before taking the lock: the main thread must never wait on a lock this thread holds.
    let for_main = wrapped.clone();
    let rtf = match on_main(HTML_IMPORT_TIMEOUT, move |mtm| {
        html_to_attributed(mtm, &for_main).and_then(|a| attributed_to_rtf(&a))
    }) {
        Ok(Ok(rtf)) if !rtf.is_empty() => Some(rtf),
        Ok(Ok(_)) => None,
        Ok(Err(e)) | Err(e) => {
            tracing::warn!(error = %e, "no RTF for rich copy; writing HTML and plain text only");
            None
        }
    };
    let _lock = pasteboard_lock();
    write_pasteboard(&wrapped, rtf.as_deref(), plain)
}

fn write_pasteboard(wrapped_html: &str, rtf: Option<&[u8]>, plain: &str) -> anyhow::Result<()> {
    autoreleasepool(|_| {
        let item = NSPasteboardItem::new();
        let html_ok = item.setData_forType(&NSData::with_bytes(wrapped_html.as_bytes()), &NSString::from_str("public.html"));
        let rtf_ok = rtf.is_none_or(|rtf| item.setData_forType(&NSData::with_bytes(rtf), &NSString::from_str("public.rtf")));
        let plain_ok = item.setString_forType(&NSString::from_str(plain), &NSString::from_str("public.utf8-plain-text"));
        if !(html_ok && rtf_ok && plain_ok) {
            bail!("NSPasteboardItem refused a type (html {html_ok}, rtf {rtf_ok}, plain {plain_ok})");
        }
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();
        let obj: Retained<ProtocolObject<dyn NSPasteboardWriting>> = ProtocolObject::from_retained(item);
        if !pb.writeObjects(&NSArray::from_retained_slice(&[obj])) {
            bail!("NSPasteboard::writeObjects failed");
        }
        Ok(())
    })
}

// ---- PDF -------------------------------------------------------------------------------------------

/// Paper for the user's region: Letter in the US and Canada, A4 everywhere else.
pub(crate) fn paper_for_region(region: Option<&str>) -> (f64, f64) {
    match region.map(|r| r.to_ascii_uppercase()) {
        Some(r) if r == "US" || r == "CA" => LETTER,
        _ => A4,
    }
}

fn current_region() -> Option<String> {
    NSLocale::currentLocale().regionCode().map(|c| c.to_string())
}

enum PdfEvent {
    Loaded,
    Done(Result<(), String>),
}

struct PdfIvars {
    id: u64,
    path: PathBuf,
    paper: NSSize,
    events: RefCell<Option<Sender<PdfEvent>>>,
    printing: Cell<bool>,
    window: RefCell<Option<Retained<NSWindow>>>,
    web_view: RefCell<Option<Retained<WKWebView>>>,
}

static NEXT_PDF_JOB: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Live PDF jobs (main thread only). WebKit holds its navigation delegate weakly.
    static PDF_JOBS: RefCell<Vec<Retained<PdfJob>>> = const { RefCell::new(Vec::new()) };
    /// The share picker being shown and its delegate (the picker holds its delegate weakly).
    static SHARE: RefCell<Option<(Retained<NSSharingServicePicker>, Retained<ShareDelegate>)>> =
        const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WrenNotesPdfJob"]
    #[ivars = PdfIvars]
    struct PdfJob;

    unsafe impl NSObjectProtocol for PdfJob {}

    unsafe impl WKNavigationDelegate for PdfJob {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, web_view: &WKWebView, _navigation: Option<&WKNavigation>) {
            self.start_print(web_view);
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.finish(Err(format!("loading the notes failed: {}", error.localizedDescription())));
        }

        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional(&self, _web_view: &WKWebView, _navigation: Option<&WKNavigation>, error: &NSError) {
            self.finish(Err(format!("loading the notes failed: {}", error.localizedDescription())));
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn content_process_ended(&self, _web_view: &WKWebView) {
            self.finish(Err("the web content process ended".into()));
        }
    }

    impl PdfJob {
        #[unsafe(method(printOperationDidRun:success:contextInfo:))]
        fn print_did_run(&self, _op: &NSPrintOperation, success: Bool, _context: *mut c_void) {
            self.finish(if success.as_bool() { Ok(()) } else { Err("the print operation failed".into()) });
        }
    }
);

impl PdfJob {
    fn send(&self, event: PdfEvent) {
        if let Some(tx) = self.ivars().events.borrow().as_ref() {
            let _ = tx.send(event);
        }
    }

    fn start_print(&self, web_view: &WKWebView) {
        if self.ivars().printing.replace(true) {
            return;
        }
        self.send(PdfEvent::Loaded);
        if let Err(e) = self.run_print(web_view) {
            self.finish(Err(e));
        }
    }

    fn run_print(&self, web_view: &WKWebView) -> Result<(), String> {
        let iv = self.ivars();
        let window = iv.window.borrow().clone().ok_or("no window for the print operation")?;
        let info = print_info(&iv.path, iv.paper)?;
        // SAFETY: main thread; the web view finished loading.
        let op = unsafe { web_view.printOperationWithPrintInfo(&info) };
        op.setShowsPrintPanel(false);
        op.setShowsProgressPanel(false);
        // WKWebView's printing view lays out to its frame: give it the printable page size.
        if let Some(view) = op.view() {
            view.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), printable_size(iv.paper)));
        }
        // The modal-for-window form is the one WebKit completes asynchronously (plain `runOperation`
        // prints blank pages, because the out-of-process page can't draw while the main thread blocks).
        // With both panels off, no sheet appears; the window itself is never shown.
        // SAFETY: `self` implements the did-run selector with the documented signature.
        unsafe {
            op.runOperationModalForWindow_delegate_didRunSelector_contextInfo(
                &window,
                Some(self.as_ref()),
                Some(sel!(printOperationDidRun:success:contextInfo:)),
                std::ptr::null_mut(),
            );
        }
        Ok(())
    }

    /// Reports the result (once) and releases the job after the current AppKit/WebKit callback returns.
    fn finish(&self, result: Result<(), String>) {
        if let Some(tx) = self.ivars().events.borrow_mut().take() {
            let _ = tx.send(PdfEvent::Done(result));
        }
        let id = self.ivars().id;
        DispatchQueue::main().exec_async(move || release_pdf_job(id));
    }

    fn tear_down(&self) {
        if let Some(web_view) = self.ivars().web_view.borrow_mut().take() {
            // SAFETY: main thread.
            unsafe {
                web_view.stopLoading();
                web_view.setNavigationDelegate(None);
            }
        }
        if let Some(window) = self.ivars().window.borrow_mut().take() {
            window.setContentView(None);
            window.close();
        }
    }
}

fn release_pdf_job(id: u64) {
    let job = PDF_JOBS.with(|jobs| {
        let mut jobs = jobs.borrow_mut();
        jobs.iter().position(|j| j.ivars().id == id).map(|i| jobs.remove(i))
    });
    if let Some(job) = job {
        job.tear_down();
    }
}

/// Worker gave up waiting. Before printing starts, drop everything now; while a print operation runs,
/// AppKit owns the window, so only stop reporting and let the did-run callback release it.
fn cancel_pdf_job(id: u64) {
    let printing = PDF_JOBS.with(|jobs| {
        jobs.borrow().iter().find(|j| j.ivars().id == id).map(|j| {
            j.ivars().events.borrow_mut().take();
            j.ivars().printing.get()
        })
    });
    if printing == Some(false) {
        release_pdf_job(id);
    }
}

fn printable_size(paper: NSSize) -> NSSize {
    NSSize::new(paper.width - 2.0 * MARGIN_PT, paper.height - 2.0 * MARGIN_PT)
}

fn print_info(path: &Path, paper: NSSize) -> Result<Retained<NSPrintInfo>, String> {
    let path_str = path.to_str().ok_or("the PDF path isn't valid UTF-8")?;
    let shared = NSPrintInfo::sharedPrintInfo();
    // SAFETY: NSPrintInfo conforms to NSCopying; `copy` returns a +1 NSPrintInfo.
    let info: Retained<NSPrintInfo> = unsafe { msg_send![&*shared, copy] };
    info.setPaperSize(paper);
    info.setTopMargin(MARGIN_PT);
    info.setBottomMargin(MARGIN_PT);
    info.setLeftMargin(MARGIN_PT);
    info.setRightMargin(MARGIN_PT);
    info.setHorizontalPagination(NSPrintingPaginationMode::Fit);
    info.setVerticalPagination(NSPrintingPaginationMode::Automatic);
    info.setHorizontallyCentered(false);
    info.setVerticallyCentered(false);
    let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
    // SAFETY: documented disposition constant; NSPrintJobSavingURL takes an NSURL.
    unsafe {
        info.setJobDisposition(NSPrintSaveJob);
        let dict = info.dictionary();
        let _: () = msg_send![&*dict, setObject: &*url, forKey: NSPrintJobSavingURL];
    }
    Ok(info)
}

/// Creates the off-screen web view and starts loading; the navigation delegate prints when it's done.
fn start_pdf_job(mtm: MainThreadMarker, id: u64, html: &str, path: PathBuf, events: Sender<PdfEvent>) -> Result<(), String> {
    let (w, h) = paper_for_region(current_region().as_deref());
    let paper = NSSize::new(w, h);
    let page = printable_size(paper);
    // SAFETY: main thread throughout.
    unsafe {
        let config = WKWebViewConfiguration::new(mtm);
        // The notes are already sanitised; no script runs while rendering them anyway.
        config.defaultWebpagePreferences().setAllowsContentJavaScript(false);
        let web_view =
            WKWebView::initWithFrame_configuration(mtm.alloc(), NSRect::new(NSPoint::new(0.0, 0.0), page), &config);
        // A borderless window far off-screen that is never ordered in: the print operation needs one.
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            NSRect::new(NSPoint::new(-20000.0, -20000.0), page),
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window.setContentView(Some(&web_view));

        let job = mtm.alloc::<PdfJob>().set_ivars(PdfIvars {
            id,
            path,
            paper,
            events: RefCell::new(Some(events)),
            printing: Cell::new(false),
            window: RefCell::new(Some(window)),
            web_view: RefCell::new(Some(web_view.clone())),
        });
        let job: Retained<PdfJob> = msg_send![super(job), init];
        web_view.setNavigationDelegate(Some(ProtocolObject::from_ref(&*job)));
        PDF_JOBS.with(|jobs| jobs.borrow_mut().push(job));
        let _ = web_view.loadHTMLString_baseURL(&NSString::from_str(html), None);
    }
    Ok(())
}

fn verify_pdf(path: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("no PDF at {}", path.display()))?;
    if !bytes.starts_with(b"%PDF-") {
        bail!("{} isn't a PDF ({} bytes)", path.display(), bytes.len());
    }
    Ok(())
}

/// Render `html` in an off-screen WKWebView and print it to `path` as a paginated PDF without a panel.
pub fn write_pdf(_app: &AppHandle, html: &str, path: &Path) -> anyhow::Result<()> {
    write_pdf_blocking(html, path)
}

/// [`write_pdf`] without the AppHandle: blocks the calling (non-main) thread until the PDF is written.
pub(crate) fn write_pdf_blocking(html: &str, path: &Path) -> anyhow::Result<()> {
    if MainThreadMarker::new().is_some() {
        bail!("write_pdf must be called off the main thread");
    }
    // The save panel already confirmed replacing it; removing it first means a stale file can't pass.
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("couldn't replace {}", path.display()));
        }
        _ => {}
    }
    let id = NEXT_PDF_JOB.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = bounded(2);
    let (html, target) = (html.to_owned(), path.to_owned());
    on_main(Duration::from_secs(5), move |mtm| start_pdf_job(mtm, id, &html, target, tx))?.map_err(|e| anyhow!(e))?;
    let cancel = move || DispatchQueue::main().exec_async(move || cancel_pdf_job(id));

    match rx.recv_timeout(PDF_LOAD_TIMEOUT) {
        Ok(PdfEvent::Loaded) => {}
        Ok(PdfEvent::Done(r)) => {
            r.map_err(|e| anyhow!(e))?;
            bail!("the PDF job ended before the notes loaded");
        }
        Err(_) => {
            cancel();
            bail!("the notes didn't load within {PDF_LOAD_TIMEOUT:?}");
        }
    }
    match rx.recv_timeout(PDF_PRINT_TIMEOUT) {
        Ok(PdfEvent::Done(r)) => r.map_err(|e| anyhow!(e))?,
        Ok(PdfEvent::Loaded) => bail!("unexpected second load event"),
        Err(_) => {
            cancel();
            bail!("printing to PDF didn't finish within {PDF_PRINT_TIMEOUT:?}");
        }
    }
    verify_pdf(path)
}

// ---- Share -----------------------------------------------------------------------------------------

struct ShareIvars {
    subject: Retained<NSString>,
    mail_title: Option<Retained<NSString>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WrenNotesShareDelegate"]
    #[ivars = ShareIvars]
    struct ShareDelegate;

    unsafe impl NSObjectProtocol for ShareDelegate {}

    unsafe impl NSSharingServicePickerDelegate for ShareDelegate {
        #[unsafe(method_id(sharingServicePicker:sharingServicesForItems:proposedSharingServices:))]
        fn services_for_items(
            &self,
            _picker: &NSSharingServicePicker,
            _items: &NSArray,
            proposed: &NSArray<NSSharingService>,
        ) -> Retained<NSArray<NSSharingService>> {
            for service in proposed.iter() {
                self.tag_mail(&service);
            }
            NSArray::from_retained_slice(&proposed.to_vec())
        }

        #[unsafe(method(sharingServicePicker:didChooseSharingService:))]
        fn did_choose(&self, _picker: &NSSharingServicePicker, service: Option<&NSSharingService>) {
            if let Some(service) = service {
                self.tag_mail(service);
            }
        }
    }
);

impl ShareDelegate {
    fn new(mtm: MainThreadMarker, subject: &str) -> Retained<Self> {
        // SAFETY: AppKit's exported constant.
        let mail = NSSharingService::sharingServiceNamed(unsafe { NSSharingServiceNameComposeEmail });
        let this = mtm.alloc::<Self>().set_ivars(ShareIvars {
            subject: NSString::from_str(subject),
            mail_title: mail.map(|m| m.title()),
        });
        // SAFETY: NSObject's designated initialiser.
        unsafe { msg_send![super(this), init] }
    }

    /// Mail is recognised by its (localised) title, the only public identity a proposed service has.
    fn tag_mail(&self, service: &NSSharingService) {
        let iv = self.ivars();
        if iv.mail_title.as_deref().is_some_and(|t| t.isEqualToString(&service.title())) {
            service.setSubject(Some(&iv.subject));
        }
    }
}

/// CSS px from the web content's top-left → a rect in `view`'s coordinates (CSS px == points at zoom 1).
pub(crate) fn anchor_rect(rect: [f64; 4], view_height: f64, flipped: bool) -> NSRect {
    let [x, y, w, h] = rect.map(|v| if v.is_finite() { v } else { 0.0 });
    let (w, h) = (w.max(1.0), h.max(1.0));
    let y = if flipped { y } else { view_height - y - h };
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

/// Shows the system Share menu below `rect` in `view`, sharing the notes as an attributed string.
pub(crate) fn show_share_picker(
    mtm: MainThreadMarker,
    view: &NSView,
    fragment: &str,
    plain: &str,
    subject: &str,
    rect: [f64; 4],
) -> anyhow::Result<()> {
    let attributed = html_to_attributed(mtm, &wrap_html(fragment)).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "sharing plain text instead of formatted notes");
        NSAttributedString::from_nsstring(&NSString::from_str(plain))
    });
    let items: Retained<NSArray> = NSArray::from_slice(&[attributed.as_ref() as &AnyObject]);
    // SAFETY: an NSAttributedString is a valid share item.
    let picker = unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
    let delegate = ShareDelegate::new(mtm, subject);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    let flipped = view.isFlipped();
    let anchor = anchor_rect(rect, view.bounds().size.height, flipped);
    // "Below the button": the low-y edge unless the view's y axis points down.
    let edge = if flipped { NSRectEdge::MaxY } else { NSRectEdge::MinY };
    picker.showRelativeToRect_ofView_preferredEdge(anchor, view, edge);
    // Replacing the previous pair releases it; only one Share menu can be open.
    SHARE.with(|s| *s.borrow_mut() = Some((picker, delegate)));
    Ok(())
}

/// Show NSSharingServicePicker anchored at `rect` (CSS px, top-left origin, main window) sharing the
/// notes as an attributed string (Mail: formatted body, `subject`).
pub fn share(app: &AppHandle, html: &str, plain: &str, subject: &str, rect: [f64; 4]) -> anyhow::Result<()> {
    let window = app.get_webview_window("main").context("the main window isn't open")?;
    let (tx, rx) = bounded(1);
    let (fragment, plain, subject) = (html.to_owned(), plain.to_owned(), subject.to_owned());
    window
        .with_webview(move |webview| {
            let result = match MainThreadMarker::new() {
                None => Err(anyhow!("with_webview didn't run on the main thread")),
                Some(_) if webview.inner().is_null() => Err(anyhow!("the main window has no web view")),
                Some(mtm) => autoreleasepool(|_| {
                    // SAFETY: on macOS `inner()` is the window's live WKWebView (an NSView).
                    let view = unsafe { &*(webview.inner() as *const NSView) };
                    show_share_picker(mtm, view, &fragment, &plain, &subject, rect)
                }),
            };
            let _ = tx.send(result);
        })
        .context("couldn't reach the main window")?;
    rx.recv_timeout(SHARE_TIMEOUT).map_err(|_| anyhow!("the Share menu didn't open within {SHARE_TIMEOUT:?}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_is_wrapped_as_utf8_document() {
        let doc = wrap_html("<p>☑ done — ok</p>");
        assert!(doc.starts_with("<html><head><meta charset=\"utf-8\"></head><body>"));
        assert!(doc.ends_with("<p>☑ done — ok</p></body></html>"));
    }

    #[test]
    fn paper_is_letter_only_in_us_and_canada() {
        assert_eq!(paper_for_region(Some("US")), LETTER);
        assert_eq!(paper_for_region(Some("ca")), LETTER);
        assert_eq!(paper_for_region(Some("GB")), A4);
        assert_eq!(paper_for_region(Some("DE")), A4);
        assert_eq!(paper_for_region(None), A4);
        let page = printable_size(NSSize::new(A4.0, A4.1));
        assert!((page.width - (A4.0 - 102.05)).abs() < 0.01, "{}", page.width);
    }

    #[test]
    fn anchor_rect_converts_top_left_css_to_view_coordinates() {
        // Non-flipped (AppKit default): y measured from the bottom.
        let r = anchor_rect([100.0, 40.0, 80.0, 24.0], 600.0, false);
        assert_eq!((r.origin.x, r.origin.y, r.size.width, r.size.height), (100.0, 536.0, 80.0, 24.0));
        // Flipped views keep the CSS y.
        let r = anchor_rect([100.0, 40.0, 80.0, 24.0], 600.0, true);
        assert_eq!((r.origin.x, r.origin.y), (100.0, 40.0));
        // Degenerate input still gives a usable 1×1 anchor.
        let r = anchor_rect([f64::NAN, 10.0, 0.0, -5.0], 100.0, false);
        assert_eq!((r.origin.x, r.origin.y, r.size.width, r.size.height), (0.0, 89.0, 1.0, 1.0));
    }

    /// Runs the real AppKit/WebKit paths on a real main thread (libtest never runs tests on it):
    /// rich copy + read-back, a 3-page PDF, and the Share picker. Output lands in /tmp/wren-notes-proof.
    #[test]
    #[ignore = "live: needs a logged-in macOS session; drives the pasteboard and WebKit via examples/notes_export_proof"]
    fn live_notes_export_proof() {
        let status = std::process::Command::new(env!("CARGO"))
            .args(["run", "--quiet", "--example", "notes_export_proof"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .status()
            .expect("run the proof example");
        assert!(status.success(), "notes_export_proof failed: {status}");
    }
}
