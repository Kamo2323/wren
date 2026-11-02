//! Clipboard work for paste (spec §7): back up every plain-memory format, publish Wren's text with
//! delayed rendering from a hidden owner window (the `WM_RENDERFORMAT` it receives is the read
//! receipt), keep it out of clipboard history / cloud clipboard, and restore the backup afterwards
//! only if nobody else has written to the clipboard since.
//!
//! The read-receipt technique was observed in Willow (docs/research/willow-teardown.md §4) and is
//! written fresh for Wren. Two details were learned from Handy's `paste_tx/windows.rs`
//! (MIT licence, Copyright (c) 2025 CJ Pais): `SetClipboardData(format, NULL)` reports success as a
//! NULL return, so the thread error must be cleared first to tell success from failure; and
//! `WM_RENDERALLFORMATS` must open the clipboard itself and check it still owns it.
//!
//! Threading: clipboard ownership and delayed rendering belong to a window, and the window's
//! thread must pump messages to answer `WM_RENDERFORMAT`. Each paste therefore runs a short-lived
//! worker thread that owns a message-only window. Only one transaction exists at a time: a new one
//! (or `copy_text`) first waits for the previous worker to finish its restore.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use parking_lot::Mutex;
use windows::Win32::Foundation::{
    ERROR_SUCCESS, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, SetLastError, WPARAM,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    GetClipboardOwner, GetClipboardSequenceNumber, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    HWND_MESSAGE, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, SetWindowLongPtrW,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_DESTROYCLIPBOARD, WM_RENDERALLFORMATS, WM_RENDERFORMAT, WM_TIMER,
    WNDCLASSW,
};
use windows::core::{PCWSTR, w};

use crate::constants::{
    CLIPBOARD_BACKUP_MAX_FORMAT_BYTES, CLIPBOARD_OPEN_RETRIES, CLIPBOARD_OPEN_RETRY_MS, CLIPBOARD_PUBLISH_TIMEOUT_MS,
    CLIPBOARD_RESTORE_MS,
};

/// Standard clipboard format numbers (winuser.h).
pub const CF_TEXT: u32 = 1;
pub const CF_BITMAP: u32 = 2;
pub const CF_METAFILEPICT: u32 = 3;
pub const CF_DIB: u32 = 8;
pub const CF_PALETTE: u32 = 9;
pub const CF_UNICODETEXT: u32 = 13;
pub const CF_ENHMETAFILE: u32 = 14;
pub const CF_HDROP: u32 = 15;
pub const CF_LOCALE: u32 = 16;
pub const CF_DIBV5: u32 = 17;
pub const CF_OWNERDISPLAY: u32 = 0x80;
pub const CF_DSPTEXT: u32 = 0x81;
pub const CF_DSPBITMAP: u32 = 0x82;
pub const CF_DSPMETAFILEPICT: u32 = 0x83;
pub const CF_DSPENHMETAFILE: u32 = 0x8E;
const CF_PRIVATE_RANGE: std::ops::RangeInclusive<u32> = 0x200..=0x2FF;
const CF_GDIOBJ_RANGE: std::ops::RangeInclusive<u32> = 0x300..=0x3FF;

/// Registered formats that tell clipboard history, cloud clipboard and monitors to leave the
/// content alone (the same ones browsers use for private-window copies), with the DWORD each holds.
const EXCLUSION_FORMATS: [(&str, u32); 3] = [
    ("ExcludeClipboardContentFromMonitorProcessing", 1),
    ("CanIncludeInClipboardHistory", 0),
    ("CanUploadToCloudClipboard", 0),
];

/// OLE's own bookkeeping formats point at a live COM object of the app that copied; restored on
/// their own they would point at nothing, so they are not backed up (the plain formats are).
const SKIPPED_REGISTERED_FORMATS: [&str; 2] = ["DataObject", "Ole Private Data"];

const WM_APP_RESTORE_LATER: u32 = WM_APP + 1;
const WM_APP_LEAVE_TEXT: u32 = WM_APP + 2;
const RESTORE_TIMER_ID: usize = 1;

// ---------------------------------------------------------------------------------------------
// Pure decisions (unit-tested without a clipboard)
// ---------------------------------------------------------------------------------------------

/// True when the handle of `format` is plain global memory that can be copied byte for byte.
/// GDI objects, metafiles, palettes, owner-display and private formats are not.
pub fn is_plain_memory_format(format: u32) -> bool {
    !(matches!(
        format,
        0 | CF_BITMAP
            | CF_METAFILEPICT
            | CF_PALETTE
            | CF_ENHMETAFILE
            | CF_OWNERDISPLAY
            | CF_DSPBITMAP
            | CF_DSPMETAFILEPICT
            | CF_DSPENHMETAFILE
    ) || CF_PRIVATE_RANGE.contains(&format)
        || CF_GDIOBJ_RANGE.contains(&format))
}

/// The restore may run only if Wren still owns the clipboard and the sequence number is one Wren
/// itself produced (publishing, and rendering, can each advance it). Anything else means another
/// app copied something after us, and that copy must win.
pub fn should_restore(ownership_lost: bool, sequence_now: u32, own_sequences: &[u32]) -> bool {
    !ownership_lost && own_sequences.contains(&sequence_now)
}

/// UTF-16 with a terminating NUL, as `CF_UNICODETEXT` wants it.
pub fn to_wide_nul(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Reads `CF_UNICODETEXT` bytes back to a string, stopping at the first NUL.
pub fn from_wide_bytes(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

// ---------------------------------------------------------------------------------------------
// Snapshot (backup) of the clipboard
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedFormat {
    pub format: u32,
    pub data: Vec<u8>,
}

/// Everything that could be backed up from the clipboard at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub formats: Vec<SavedFormat>,
}

impl Snapshot {
    /// Backs up the clipboard now (opens and closes it).
    pub fn capture() -> anyhow::Result<Snapshot> {
        let _lock = transaction_lock();
        with_owner_window(|hwnd| {
            let _open = OpenGuard::open(hwnd)?;
            Ok(unsafe { capture_open() })
        })
    }

    /// Puts this snapshot back as the whole clipboard (an empty snapshot empties it).
    pub fn restore(&self) -> anyhow::Result<()> {
        let _lock = transaction_lock();
        with_owner_window(|hwnd| {
            let _open = OpenGuard::open(hwnd)?;
            unsafe { write_formats_open(&self.formats) }
        })
    }

    pub fn get(&self, format: u32) -> Option<&[u8]> {
        self.formats.iter().find(|f| f.format == format).map(|f| f.data.as_slice())
    }
}

/// Reads every backed-up format. The clipboard must be open.
unsafe fn capture_open() -> Snapshot {
    let mut formats = Vec::new();
    let mut format = 0u32;
    loop {
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        if !is_plain_memory_format(format) || is_skipped_registered(format) {
            continue;
        }
        let Ok(handle) = (unsafe { GetClipboardData(format) }) else {
            continue;
        };
        if handle.is_invalid() {
            continue;
        }
        let hg = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(hg) };
        if size == 0 || size > CLIPBOARD_BACKUP_MAX_FORMAT_BYTES {
            if size > CLIPBOARD_BACKUP_MAX_FORMAT_BYTES {
                tracing::warn!(format, size, "clipboard format too large to back up; it will not be restored");
            }
            continue;
        }
        let ptr = unsafe { GlobalLock(hg) } as *const u8;
        if ptr.is_null() {
            continue;
        }
        let data = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
        let _ = unsafe { GlobalUnlock(hg) };
        formats.push(SavedFormat { format, data });
    }
    Snapshot { formats }
}

fn is_skipped_registered(format: u32) -> bool {
    if format < 0xC000 {
        return false;
    }
    let mut buf = [0u16; 128];
    let n = unsafe { GetClipboardFormatNameW(format, &mut buf) };
    if n <= 0 {
        return false;
    }
    let name = String::from_utf16_lossy(&buf[..n as usize]);
    SKIPPED_REGISTERED_FORMATS.contains(&name.as_str())
}

/// Empties the clipboard and writes `formats`. The clipboard must be open by an owner window.
///
/// The exclusion formats go on first: putting the user's own earlier copy back is not a new copy,
/// so clipboard history must not record it again (measured: without this every restore added a
/// duplicate entry to Win+V history and pushed older entries out). A saved format of the same name
/// (the original copy was itself excluded) is written afterwards and wins.
unsafe fn write_formats_open(formats: &[SavedFormat]) -> anyhow::Result<()> {
    unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
    if formats.is_empty() {
        return Ok(()); // it was empty: leave it empty
    }
    unsafe { set_exclusion_formats() };
    let mut failed = 0;
    for saved in formats {
        if unsafe { set_bytes(saved.format, &saved.data) }.is_err() {
            failed += 1;
        }
    }
    if failed > 0 {
        tracing::warn!(failed, total = formats.len(), "some clipboard formats could not be restored");
    }
    Ok(())
}

/// Copies `data` into new global memory and hands it to the clipboard (which must be open).
unsafe fn set_bytes(format: u32, data: &[u8]) -> anyhow::Result<()> {
    let hg = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len().max(1)) }.context("GlobalAlloc failed")?;
    let ptr = unsafe { GlobalLock(hg) } as *mut u8;
    if ptr.is_null() {
        let _ = unsafe { GlobalFree(Some(hg)) };
        bail!("GlobalLock failed");
    }
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len()) };
    let _ = unsafe { GlobalUnlock(hg) };
    // On success the clipboard owns the memory; on failure it is still ours to free.
    if let Err(e) = unsafe { SetClipboardData(format, Some(HANDLE(hg.0))) } {
        let _ = unsafe { GlobalFree(Some(hg)) };
        return Err(anyhow!("SetClipboardData({format}) failed: {e}"));
    }
    Ok(())
}

fn wide_bytes(text: &str) -> Vec<u8> {
    to_wide_nul(text).iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// Writes the three exclusion formats. The clipboard must be open and already emptied by us.
unsafe fn set_exclusion_formats() {
    for (name, value) in EXCLUSION_FORMATS {
        let wide = to_wide_nul(name);
        let format = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
        if format == 0 {
            tracing::warn!(name, "RegisterClipboardFormat failed");
            continue;
        }
        if let Err(e) = unsafe { set_bytes(format, &value.to_le_bytes()) } {
            tracing::warn!(name, error = %e, "could not set clipboard exclusion format");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Opening the clipboard, owner windows, serialisation
// ---------------------------------------------------------------------------------------------

/// Closes the clipboard when dropped.
struct OpenGuard;

impl OpenGuard {
    /// `OpenClipboard` with `CLIPBOARD_OPEN_RETRIES` tries `CLIPBOARD_OPEN_RETRY_MS` apart (another
    /// app may hold it for a moment).
    fn open(hwnd: HWND) -> anyhow::Result<OpenGuard> {
        let mut last_err = None;
        for attempt in 0..CLIPBOARD_OPEN_RETRIES {
            match unsafe { OpenClipboard(Some(hwnd)) } {
                Ok(()) => return Ok(OpenGuard),
                Err(e) => last_err = Some(e),
            }
            if attempt + 1 < CLIPBOARD_OPEN_RETRIES {
                std::thread::sleep(Duration::from_millis(CLIPBOARD_OPEN_RETRY_MS));
            }
        }
        Err(anyhow!(
            "the clipboard is busy (another app holds it): {}",
            last_err.map(|e| e.to_string()).unwrap_or_default()
        ))
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

/// The worker of the previous transaction, if it may still be running (restore pending).
static PENDING: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
/// Held for the whole of every clipboard operation so two never interleave.
static TRANSACTION: Mutex<()> = Mutex::new(());

/// Waits for any pending restore, then locks out other clipboard operations.
fn transaction_lock() -> parking_lot::MutexGuard<'static, ()> {
    let guard = TRANSACTION.lock();
    flush_pending();
    guard
}

fn flush_pending() {
    let pending = PENDING.lock().take();
    if let Some(handle) = pending {
        let _ = handle.join();
    }
}

/// Waits until the last paste's worker has restored the clipboard (or decided not to). Call on quit.
pub fn flush() {
    let _lock = TRANSACTION.lock();
    flush_pending();
}

fn hinstance() -> anyhow::Result<HINSTANCE> {
    Ok(unsafe { GetModuleHandleW(None) }.context("GetModuleHandle failed")?.into())
}

const CLASS_NAME: PCWSTR = w!("WrenClipboardOwner");

fn ensure_class() -> anyhow::Result<()> {
    static ONCE: Once = Once::new();
    let instance = hinstance()?;
    ONCE.call_once(|| {
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance, lpszClassName: CLASS_NAME, ..Default::default() };
        unsafe { RegisterClassW(&wc) };
    });
    Ok(())
}

fn create_owner_window() -> anyhow::Result<HWND> {
    ensure_class()?;
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS_NAME,
            w!("Wren clipboard"),
            WINDOW_STYLE::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance()?),
            None,
        )
    }
    .context("CreateWindowEx (clipboard owner) failed")?;
    Ok(hwnd)
}

/// Runs `f` with a temporary message-only window on this thread to own the clipboard (with a NULL
/// owner, `EmptyClipboard` leaves no owner and `SetClipboardData` is not reliable).
fn with_owner_window<T>(f: impl FnOnce(HWND) -> anyhow::Result<T>) -> anyhow::Result<T> {
    let hwnd = create_owner_window()?;
    let result = f(hwnd);
    let _ = unsafe { DestroyWindow(hwnd) };
    result
}

// ---------------------------------------------------------------------------------------------
// Plain copy and read
// ---------------------------------------------------------------------------------------------

/// Puts `text` on the clipboard as ordinary data (not delayed), excluded from clipboard history and
/// cloud clipboard. Used for "Not pasted — click to copy" and History's copy button.
pub fn copy_text(text: &str) -> anyhow::Result<()> {
    if text.is_empty() {
        bail!("nothing to copy");
    }
    let _lock = transaction_lock();
    with_owner_window(|hwnd| {
        let _open = OpenGuard::open(hwnd)?;
        unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
        unsafe { set_exclusion_formats() };
        unsafe { set_bytes(CF_UNICODETEXT, &wide_bytes(text)) }
    })
}

/// The clipboard's text, if it has any. (Reading a delayed-rendered format asks its owner to
/// render it, so do not call this on Wren's own pending paste.)
pub fn read_text() -> Option<String> {
    let _lock = transaction_lock();
    read_text_unlocked()
}

/// `read_text` without taking the transaction lock: what another app reading the clipboard does.
fn read_text_unlocked() -> Option<String> {
    with_owner_window(|hwnd| {
        let _open = OpenGuard::open(hwnd)?;
        let handle = unsafe { GetClipboardData(CF_UNICODETEXT) }?;
        let hg = HGLOBAL(handle.0);
        let size = unsafe { GlobalSize(hg) };
        let ptr = unsafe { GlobalLock(hg) } as *const u8;
        if ptr.is_null() {
            bail!("GlobalLock failed");
        }
        let text = from_wide_bytes(unsafe { std::slice::from_raw_parts(ptr, size) });
        let _ = unsafe { GlobalUnlock(hg) };
        Ok(text)
    })
    .ok()
}

/// The clipboard sequence number (changes whenever anyone writes to the clipboard).
pub fn sequence_number() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

// ---------------------------------------------------------------------------------------------
// The paste transaction (delayed rendering + read receipt)
// ---------------------------------------------------------------------------------------------

/// What the worker did when it finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    /// The backup was put back.
    Restored,
    /// Someone else wrote to the clipboard after Wren; their content was left alone.
    SkippedChangedByOthers,
    /// Wren's text was left on the clipboard (not read, or focus moved).
    LeftText,
    /// Restoring failed (logged).
    RestoreFailed,
}

struct Shared {
    text: Vec<u8>,
    read_at: Mutex<Option<Instant>>,
    read: AtomicBool,
    ownership_lost: AtomicBool,
    /// Set while Wren itself empties the clipboard, so its own `WM_DESTROYCLIPBOARD` is ignored.
    self_emptying: AtomicBool,
    /// The delayed-render promise is on the clipboard and not yet rendered. Windows sends
    /// `WM_RENDERALLFORMATS` to a destroyed owner even after a restore, so this guards it.
    promise_pending: AtomicBool,
    own_sequences: Mutex<Vec<u32>>,
    backup: Mutex<Snapshot>,
    finish: Mutex<Option<Finish>>,
}

/// One paste's hold on the clipboard. Created by `Transaction::start`, ended by
/// `restore_later` (after a read) or `leave_text` (no read); dropping it leaves the text.
pub struct Transaction {
    hwnd: isize,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    _lock: parking_lot::MutexGuard<'static, ()>,
}

impl Transaction {
    /// Backs up the clipboard and publishes `text` as a delayed-render promise plus the exclusion
    /// formats. When this returns Ok, the promise is on the clipboard and nothing has read it yet.
    pub fn start(text: &str) -> anyhow::Result<Transaction> {
        let lock = transaction_lock();
        let shared = Arc::new(Shared {
            text: wide_bytes(text),
            read_at: Mutex::new(None),
            read: AtomicBool::new(false),
            ownership_lost: AtomicBool::new(false),
            self_emptying: AtomicBool::new(false),
            promise_pending: AtomicBool::new(false),
            own_sequences: Mutex::new(Vec::new()),
            backup: Mutex::new(Snapshot::default()),
            finish: Mutex::new(None),
        });
        let (tx, rx) = crossbeam_channel::bounded::<anyhow::Result<isize>>(1);
        let for_worker = shared.clone();
        let worker = std::thread::Builder::new()
            .name("wren-clipboard".into())
            .spawn(move || worker(for_worker, tx))
            .context("cannot start the clipboard thread")?;
        match rx.recv_timeout(Duration::from_millis(CLIPBOARD_PUBLISH_TIMEOUT_MS)) {
            Ok(Ok(hwnd)) => Ok(Transaction { hwnd, shared, worker: Some(worker), _lock: lock }),
            Ok(Err(e)) => {
                let _ = worker.join();
                Err(e)
            }
            Err(_) => {
                // The worker is stuck (clipboard hung by another app). It is left to finish on its
                // own: if it does publish later, it sees nobody is waiting and restores the backup.
                drop(worker);
                Err(anyhow!("the clipboard did not respond in time"))
            }
        }
    }

    /// True once the target app has asked for the text (`WM_RENDERFORMAT`).
    pub fn was_read(&self) -> bool {
        self.shared.read.load(Ordering::SeqCst)
    }

    pub fn read_at(&self) -> Option<Instant> {
        *self.shared.read_at.lock()
    }

    /// Polls `was_read` every `poll` until `max`; true if it was read.
    pub fn wait_read(&self, max: Duration, poll: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.was_read() {
                return true;
            }
            if start.elapsed() >= max {
                return false;
            }
            std::thread::sleep(poll);
        }
    }

    /// After a read: restore the backup `CLIPBOARD_RESTORE_MS` from now (on the worker thread, so
    /// the caller is not held up), if nobody else wrote to the clipboard meanwhile.
    pub fn restore_later(mut self) {
        self.end(WM_APP_RESTORE_LATER);
    }

    /// No read: render Wren's text for good and leave it on the clipboard ("Copied — press Ctrl+V").
    /// Waits until that is done.
    pub fn leave_text(mut self) {
        self.end(WM_APP_LEAVE_TEXT);
        flush_pending();
    }

    fn end(&mut self, message: u32) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        let hwnd = HWND(self.hwnd as *mut _);
        if unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)) }.is_err() {
            tracing::warn!("could not message the clipboard thread");
        }
        *PENDING.lock() = Some(worker);
    }

    /// A handle for reading how this transaction ends, usable after it is consumed.
    pub fn outcome(&self) -> Outcome {
        Outcome(self.shared.clone())
    }
}

impl Drop for Transaction {
    fn drop(&mut self) {
        // Ended without a decision (early return, panic): keep the text rather than lose it.
        self.end(WM_APP_LEAVE_TEXT);
    }
}

/// How a transaction ended (diagnostics for tests and the probe). Call `flush` first to wait.
#[derive(Clone)]
pub struct Outcome(Arc<Shared>);

impl Outcome {
    pub fn finish(&self) -> Option<Finish> {
        *self.0.finish.lock()
    }
    pub fn read_at(&self) -> Option<Instant> {
        *self.0.read_at.lock()
    }
}

fn worker(shared: Arc<Shared>, ready: crossbeam_channel::Sender<anyhow::Result<isize>>) {
    let hwnd = match create_owner_window() {
        Ok(h) => h,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let raw = Arc::into_raw(shared.clone());
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, raw as isize) };

    let published: anyhow::Result<()> = (|| {
        let _open = OpenGuard::open(hwnd)?;
        let backup = unsafe { capture_open() };
        *shared.backup.lock() = backup;
        shared.self_emptying.store(true, Ordering::SeqCst);
        let result = unsafe { publish_open() };
        shared.self_emptying.store(false, Ordering::SeqCst);
        shared.promise_pending.store(result.is_ok(), Ordering::SeqCst);
        if let Err(e) = result {
            // Put the user's clipboard back before giving up.
            let backup = shared.backup.lock().clone();
            shared.self_emptying.store(true, Ordering::SeqCst);
            let _ = unsafe { write_formats_open(&backup.formats) };
            shared.self_emptying.store(false, Ordering::SeqCst);
            return Err(e);
        }
        Ok(())
    })();

    match published {
        Ok(()) => {
            shared.own_sequences.lock().push(unsafe { GetClipboardSequenceNumber() });
            if ready.send(Ok(hwnd.0 as isize)).is_err() {
                // The caller gave up waiting and reported a failure: put the backup back.
                let finish = unsafe { restore(hwnd, &shared) };
                *shared.finish.lock() = Some(finish);
                let _ = unsafe { DestroyWindow(hwnd) };
                drop(unsafe { Arc::from_raw(raw) });
                return;
            }
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                unsafe { DispatchMessageW(&msg) };
            }
        }
        Err(e) => {
            let _ = ready.send(Err(e));
        }
    }

    // DestroyWindow sends WM_RENDERALLFORMATS if a promise is still ours, so the window proc needs
    // `shared` until the window is gone.
    let _ = unsafe { DestroyWindow(hwnd) };
    drop(unsafe { Arc::from_raw(raw) });
}

/// Empties the clipboard, adds the exclusion formats and promises `CF_UNICODETEXT` (NULL handle =
/// delayed rendering). The clipboard must be open by the owner window.
unsafe fn publish_open() -> anyhow::Result<()> {
    unsafe { EmptyClipboard() }.context("EmptyClipboard failed")?;
    unsafe { set_exclusion_formats() };
    // A NULL handle comes back as "failure" from the windows crate; only a real thread error is one.
    unsafe { SetLastError(ERROR_SUCCESS) };
    if let Err(e) = unsafe { SetClipboardData(CF_UNICODETEXT, None) }
        && e.code().is_err()
    {
        bail!("SetClipboardData (delayed) failed: {e}");
    }
    Ok(())
}

unsafe fn shared_of(hwnd: HWND) -> Option<&'static Shared> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Shared;
    // SAFETY: set by `worker` before any message that reads it, cleared only after DestroyWindow.
    unsafe { ptr.as_ref() }
}

/// Hands the text to the clipboard (which is open: by the system for WM_RENDERFORMAT, by us otherwise).
unsafe fn render(shared: &Shared) {
    shared.promise_pending.store(false, Ordering::SeqCst);
    if let Err(e) = unsafe { set_bytes(CF_UNICODETEXT, &shared.text) } {
        tracing::warn!(error = %e, "rendering the clipboard text failed");
    }
    shared.own_sequences.lock().push(unsafe { GetClipboardSequenceNumber() });
}

/// Renders the promise if this window still owns the clipboard (it opens the clipboard itself).
unsafe fn render_if_still_owner(hwnd: HWND, shared: &Shared) -> bool {
    if !shared.promise_pending.load(Ordering::SeqCst) {
        return false;
    }
    let Ok(_open) = OpenGuard::open(hwnd) else {
        return false;
    };
    let owner = unsafe { GetClipboardOwner() }.ok();
    if owner == Some(hwnd) {
        unsafe { render(shared) };
        true
    } else {
        false
    }
}

unsafe fn restore(hwnd: HWND, shared: &Shared) -> Finish {
    let now = unsafe { GetClipboardSequenceNumber() };
    let lost = shared.ownership_lost.load(Ordering::SeqCst);
    if !should_restore(lost, now, &shared.own_sequences.lock()) {
        tracing::info!("clipboard changed by another app since the paste; not restoring");
        return Finish::SkippedChangedByOthers;
    }
    let backup = shared.backup.lock().clone();
    let result = OpenGuard::open(hwnd).and_then(|_open| {
        shared.self_emptying.store(true, Ordering::SeqCst);
        let r = unsafe { write_formats_open(&backup.formats) };
        shared.self_emptying.store(false, Ordering::SeqCst);
        if r.is_ok() {
            shared.promise_pending.store(false, Ordering::SeqCst);
        }
        r
    });
    match result {
        Ok(()) => {
            tracing::info!(formats = backup.formats.len(), "clipboard restored");
            Finish::Restored
        }
        Err(e) => {
            tracing::warn!(error = %e, "clipboard restore failed");
            Finish::RestoreFailed
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(shared) = (unsafe { shared_of(hwnd) }) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_RENDERFORMAT => {
            if wparam.0 as u32 == CF_UNICODETEXT {
                // The read receipt: an app is reading the text right now.
                shared.read_at.lock().get_or_insert_with(Instant::now);
                shared.read.store(true, Ordering::SeqCst);
                unsafe { render(shared) };
            }
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            // The window is going away with the promise unread; keep the text (not a receipt).
            unsafe { render_if_still_owner(hwnd, shared) };
            LRESULT(0)
        }
        WM_DESTROYCLIPBOARD => {
            if !shared.self_emptying.load(Ordering::SeqCst) {
                shared.ownership_lost.store(true, Ordering::SeqCst);
                shared.promise_pending.store(false, Ordering::SeqCst);
            }
            LRESULT(0)
        }
        WM_APP_RESTORE_LATER => {
            unsafe { SetTimer(Some(hwnd), RESTORE_TIMER_ID, CLIPBOARD_RESTORE_MS as u32, None) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == RESTORE_TIMER_ID => {
            let _ = unsafe { KillTimer(Some(hwnd), RESTORE_TIMER_ID) };
            let finish = unsafe { restore(hwnd, shared) };
            *shared.finish.lock() = Some(finish);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_APP_LEAVE_TEXT => {
            if !shared.read.load(Ordering::SeqCst) && !shared.ownership_lost.load(Ordering::SeqCst) {
                unsafe { render_if_still_owner(hwnd, shared) };
            }
            *shared.finish.lock() = Some(Finish::LeftText);
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_memory_formats() {
        for f in [CF_TEXT, CF_DIB, CF_UNICODETEXT, CF_HDROP, CF_LOCALE, CF_DIBV5, CF_DSPTEXT, 0xC001, 0xC2A0] {
            assert!(is_plain_memory_format(f), "{f:#x}");
        }
        for f in [
            0,
            CF_BITMAP,
            CF_METAFILEPICT,
            CF_PALETTE,
            CF_ENHMETAFILE,
            CF_OWNERDISPLAY,
            CF_DSPBITMAP,
            CF_DSPMETAFILEPICT,
            CF_DSPENHMETAFILE,
            0x200,
            0x2FF,
            0x300,
            0x3FF,
        ] {
            assert!(!is_plain_memory_format(f), "{f:#x}");
        }
    }

    #[test]
    fn restore_only_when_nobody_else_wrote() {
        assert!(should_restore(false, 41, &[41]));
        assert!(should_restore(false, 42, &[41, 42])); // rendering advanced the number
        assert!(!should_restore(false, 43, &[41, 42])); // another app copied after us
        assert!(!should_restore(true, 42, &[41, 42])); // another app emptied the clipboard
        assert!(!should_restore(false, 41, &[]));
    }

    #[test]
    fn wide_round_trip() {
        let text = "Hello — ünïcödé 日本語 ✓ 🎉";
        let bytes = wide_bytes(text);
        assert_eq!(bytes.len() % 2, 0);
        assert_eq!(&bytes[bytes.len() - 2..], &[0, 0]);
        assert_eq!(from_wide_bytes(&bytes), text);
        // Extra bytes after the NUL (GlobalSize can round up) are ignored.
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0x41, 0, 0x42, 0]);
        assert_eq!(from_wide_bytes(&padded), text);
    }

    #[test]
    fn copy_text_refuses_empty() {
        assert!(copy_text("").is_err());
    }
}

/// Real-clipboard tests. They change the desktop clipboard (saved and put back by each test), so
/// they are ignored by default: `cargo test --lib paste -- --ignored --test-threads=1`.
#[cfg(test)]
mod desktop_tests {
    use super::*;
    use std::hash::{Hash, Hasher};
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;

    /// Saves the user's clipboard and puts it back when dropped.
    struct UserClipboard(Snapshot);
    impl UserClipboard {
        fn save() -> Self {
            UserClipboard(Snapshot::capture().expect("back up the user's clipboard"))
        }
    }
    impl Drop for UserClipboard {
        fn drop(&mut self) {
            flush();
            self.0.restore().expect("put the user's clipboard back");
        }
    }

    /// A 16×16 32-bit DIB (BITMAPINFOHEADER + pixels) with a recognisable pattern.
    pub(crate) fn test_dib() -> Vec<u8> {
        let (w, h) = (16i32, 16i32);
        let mut v = Vec::new();
        v.extend_from_slice(&40u32.to_le_bytes());
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&32u16.to_le_bytes());
        v.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        v.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
        v.extend_from_slice(&[0u8; 16]); // resolution, colours
        for i in 0..(w * h) {
            v.extend_from_slice(&[(i * 7) as u8, (i * 3) as u8, (255 - i) as u8, 255]);
        }
        v
    }

    fn hash(bytes: &[u8]) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        h.finish()
    }

    fn put_image_and_text() {
        Snapshot { formats: vec![SavedFormat { format: CF_DIB, data: test_dib() }, SavedFormat { format: CF_UNICODETEXT, data: wide_bytes("user text") }] }
            .restore()
            .unwrap();
    }

    /// Another app writing to the clipboard (no Wren lock). Marked excluded so the test does not
    /// add entries to the user's clipboard history.
    fn other_app_copies(text: &str) {
        with_owner_window(|hwnd| {
            let _open = OpenGuard::open(hwnd)?;
            unsafe { EmptyClipboard() }?;
            unsafe { set_exclusion_formats() };
            unsafe { set_bytes(CF_UNICODETEXT, &wide_bytes(text)) }
        })
        .unwrap();
    }

    fn excluded_formats_present() -> bool {
        EXCLUSION_FORMATS.iter().all(|(name, _)| {
            let wide = to_wide_nul(name);
            let f = unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) };
            unsafe { IsClipboardFormatAvailable(f) }.is_ok()
        })
    }

    #[test]
    #[ignore = "uses the real desktop clipboard"]
    fn receipt_then_restore_puts_the_image_back() {
        let _user = UserClipboard::save();
        put_image_and_text();
        let before = Snapshot::capture().unwrap();
        let image_hash = hash(before.get(CF_DIB).unwrap());

        let tx = Transaction::start("Dictated words").unwrap();
        assert!(excluded_formats_present(), "exclusion formats missing");
        assert!(!tx.wait_read(Duration::from_millis(300), Duration::from_millis(10)), "spurious read with no paste");
        let outcome = tx.outcome();
        let started = Instant::now();
        let reader = std::thread::spawn(read_text_unlocked);
        assert!(tx.wait_read(Duration::from_millis(1000), Duration::from_millis(10)));
        let latency = tx.read_at().unwrap().duration_since(started);
        assert_eq!(reader.join().unwrap().as_deref(), Some("Dictated words"));
        tx.restore_later();
        flush();
        assert_eq!(outcome.finish(), Some(Finish::Restored));
        let after = Snapshot::capture().unwrap();
        assert_eq!(hash(after.get(CF_DIB).unwrap()), image_hash, "image changed");
        assert_eq!(from_wide_bytes(after.get(CF_UNICODETEXT).unwrap()), "user text");
        println!("read receipt latency (simulated reader): {} ms; image hash {image_hash:016x}", latency.as_millis());
    }

    #[test]
    #[ignore = "uses the real desktop clipboard"]
    fn restore_waits_the_restore_delay() {
        let _user = UserClipboard::save();
        put_image_and_text();
        let tx = Transaction::start("Delayed").unwrap();
        let reader = std::thread::spawn(read_text_unlocked);
        assert!(tx.wait_read(Duration::from_millis(1000), Duration::from_millis(10)));
        reader.join().unwrap();
        let ended = Instant::now();
        tx.restore_later();
        std::thread::sleep(Duration::from_millis(CLIPBOARD_RESTORE_MS / 2));
        // Still Wren's text half-way through the delay (read without the lock, as another app would).
        assert_eq!(read_text_unlocked().as_deref(), Some("Delayed"));
        flush();
        let waited = ended.elapsed().as_millis() as u64;
        assert!(waited >= CLIPBOARD_RESTORE_MS, "restored after {waited} ms");
        assert_eq!(read_text().as_deref(), Some("user text"));
    }

    #[test]
    #[ignore = "uses the real desktop clipboard"]
    fn unread_text_is_left_on_the_clipboard() {
        let _user = UserClipboard::save();
        put_image_and_text();
        let tx = Transaction::start("Nobody read me").unwrap();
        assert!(!tx.wait_read(Duration::from_millis(1000), Duration::from_millis(10)), "spurious read (clipboard monitor?)");
        let outcome = tx.outcome();
        tx.leave_text();
        assert_eq!(outcome.finish(), Some(Finish::LeftText));
        // The worker is gone; the text must still be there, as real data.
        assert_eq!(read_text().as_deref(), Some("Nobody read me"));
        assert!(excluded_formats_present());
    }

    #[test]
    #[ignore = "uses the real desktop clipboard"]
    fn a_copy_by_another_app_after_the_read_wins() {
        let _user = UserClipboard::save();
        put_image_and_text();
        let tx = Transaction::start("Mine").unwrap();
        let outcome = tx.outcome();
        let reader = std::thread::spawn(read_text_unlocked);
        assert!(tx.wait_read(Duration::from_millis(1000), Duration::from_millis(10)));
        reader.join().unwrap();
        tx.restore_later();
        other_app_copies("Their new copy");
        flush();
        assert_eq!(outcome.finish(), Some(Finish::SkippedChangedByOthers));
        assert_eq!(read_text().as_deref(), Some("Their new copy"));
    }

    #[test]
    #[ignore = "uses the real desktop clipboard"]
    fn copy_text_is_plain_and_excluded() {
        let _user = UserClipboard::save();
        copy_text("Copied by Wren — ünïcödé").unwrap();
        assert_eq!(read_text().as_deref(), Some("Copied by Wren — ünïcödé"));
        assert!(excluded_formats_present());
    }

    #[test]
    #[ignore = "uses the real desktop clipboard"]
    fn empty_clipboard_is_restored_as_empty() {
        let _user = UserClipboard::save();
        Snapshot::default().restore().unwrap();
        let tx = Transaction::start("Into the void").unwrap();
        let reader = std::thread::spawn(read_text_unlocked);
        assert!(tx.wait_read(Duration::from_millis(1000), Duration::from_millis(10)));
        reader.join().unwrap();
        tx.restore_later();
        flush();
        assert_eq!(read_text(), None);
    }
}
