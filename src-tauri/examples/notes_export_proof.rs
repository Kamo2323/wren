//! Live proof for the macOS notes export (src/notes_export/macos.rs) on a real main thread, which libtest
//! can't give a unit test:
//!
//!   cargo run --example notes_export_proof              # rich copy + read-back, 3-page PDF
//!   cargo run --example notes_export_proof -- --share   # …and show the Share picker in a test window
//!
//! Copy goes through the real command (`wren_lib::notes_export::notes_copy_rich`, real paste lock). The
//! PDF and Share code is the same file compiled into this binary (`#[path]`), because `macos` is private
//! to `notes_export`. Everything is written to /tmp/wren-notes-proof. Leaves the sample notes on the
//! clipboard (that's the point: paste them somewhere to look).

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("notes_export_proof is macOS-only");
}

#[cfg(target_os = "macos")]
#[path = "../src/notes_export/macos.rs"]
#[allow(dead_code)]
mod macos;

/// Stand-in for the crate path `macos.rs` uses for the paste lock (the real one is `pub(crate)`; the copy
/// proof below goes through the library, which uses the real lock).
#[cfg(target_os = "macos")]
mod paste {
    pub mod clipboard_macos {
        static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        pub fn transaction_lock() -> parking_lot::MutexGuard<'static, ()> {
            LOCK.lock()
        }
    }
}

#[cfg(target_os = "macos")]
const FRAGMENT: &str = r#"<div style="font-family:-apple-system,Helvetica,Arial,sans-serif;font-size:14px;color:#1d1d1f;line-height:1.45">
<h1 style="font-size:22px;margin:0 0 4px">Q3 roadmap review — Wren 🐦</h1>
<p style="margin:0 0 12px;color:#6e6e73">25 Nov 2026 · 42 min · Detailed notes</p>
<h2 style="font-size:17px;margin:16px 0 6px">Decisions</h2>
<ul style="margin:0 0 10px;padding-left:22px">
<li>Ship <strong>notes export</strong> on the Mac first — Windows follows.</li>
<li>Keep the <em>self-signed</em> certificate; see <a href="https://example.com/signing">signing notes</a>.</li>
</ul>
<h2 style="font-size:17px;margin:16px 0 6px">Actions</h2>
<p style="margin:0 0 4px">☐ Draft the release notes<br>☑ Book the follow-up</p>
<table style="border-collapse:collapse;margin:8px 0;font-size:13px">
<thead><tr><th style="border:1px solid #c7c7cc;padding:4px 8px;text-align:left;background:#f2f2f7">Owner</th><th style="border:1px solid #c7c7cc;padding:4px 8px;text-align:left;background:#f2f2f7">Action</th><th style="border:1px solid #c7c7cc;padding:4px 8px;text-align:left;background:#f2f2f7">Due</th></tr></thead>
<tbody><tr><td style="border:1px solid #c7c7cc;padding:4px 8px">Maximilian</td><td style="border:1px solid #c7c7cc;padding:4px 8px">Test the PDF export</td><td style="border:1px solid #c7c7cc;padding:4px 8px">Fri</td></tr>
<tr><td style="border:1px solid #c7c7cc;padding:4px 8px">Dad</td><td style="border:1px solid #c7c7cc;padding:4px 8px">Re-grant permissions</td><td style="border:1px solid #c7c7cc;padding:4px 8px">Mon</td></tr></tbody>
</table>
<blockquote style="margin:8px 0;padding-left:10px;border-left:3px solid #c7c7cc;color:#3a3a3c">“Formatted everywhere” — the brief.</blockquote>
</div>"#;

#[cfg(target_os = "macos")]
const PLAIN: &str = "Q3 roadmap review — Wren 🐦\n25 Nov 2026 · 42 min · Detailed notes\n\nDecisions\n• Ship notes export on the Mac first — Windows follows.\n• Keep the self-signed certificate; see signing notes (https://example.com/signing).\n\nActions\n☐ Draft the release notes\n☑ Book the follow-up\n\nOwner: Maximilian · Action: Test the PDF export · Due: Fri\nOwner: Dad · Action: Re-grant permissions · Due: Mon\n\n“Formatted everywhere” — the brief.\n";

/// A complete export document long enough for ~3 A4 pages, with the print rules the page uses.
#[cfg(target_os = "macos")]
fn pdf_document() -> String {
    let mut body = String::from(
        r#"<h1>Quarterly planning — Wren 🐦</h1><p class="meta">25 Nov 2026 · 58 min · Detailed notes</p>"#,
    );
    for s in 1..=6 {
        body.push_str(&format!("<h2>Section {s}: topic discussed</h2>"));
        body.push_str("<p>The team walked through the current state — what shipped, what slipped and why. ");
        body.push_str("Several people raised the same concern about clipboard behaviour during dictation, so the ");
        body.push_str("group agreed to keep the paste lock and test it again on both Macs before the release.</p><ul>");
        for i in 1..=5 {
            body.push_str(&format!(
                "<li><strong>Point {s}.{i}</strong> — a bullet long enough to wrap onto a second line so that page \
                 breaks inside list items can be checked; it must never be cut in half.</li>"
            ));
        }
        body.push_str("</ul><p>☐ Follow up on the open question<br>☑ Agreed next step recorded</p>");
    }
    body.push_str("<h2>Action items</h2><table><thead><tr><th>Owner</th><th>Action</th><th>Due</th></tr></thead><tbody>");
    for r in 1..=24 {
        body.push_str(&format!(
            "<tr><td>Person {r}</td><td>Action number {r}: check the exported notes look right in Mail and Preview</td><td>Day {r}</td></tr>"
        ));
    }
    body.push_str("</tbody></table><blockquote>“Formatted everywhere” — the brief.</blockquote>");
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Notes</title><style>
@page {{ size: auto; }}
html, body {{ margin: 0; padding: 0; }}
body {{ font: 11pt/1.45 -apple-system, "Helvetica Neue", Arial, sans-serif; color: #1d1d1f; }}
h1 {{ font-size: 20pt; margin: 0 0 2pt; }}
.meta {{ color: #6e6e73; margin: 0 0 12pt; }}
h2 {{ font-size: 14pt; margin: 16pt 0 6pt; break-after: avoid; page-break-after: avoid; }}
p, li {{ orphans: 3; widows: 3; }}
li, tr, blockquote {{ break-inside: avoid; page-break-inside: avoid; }}
table {{ border-collapse: collapse; width: 100%; font-size: 10pt; }}
th, td {{ border: 1px solid #8e8e93; padding: 4pt 6pt; text-align: left; vertical-align: top; }}
th {{ background: #f2f2f7; }}
thead {{ display: table-header-group; }}
blockquote {{ margin: 10pt 0; padding-left: 10pt; border-left: 3px solid #c7c7cc; color: #3a3a3c; }}
</style></head><body>{body}</body></html>"#
    )
}

#[cfg(target_os = "macos")]
fn main() {
    use std::time::Instant;

    use dispatch2::DispatchQueue;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSPasteboard, NSWindow, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let share = std::env::args().any(|a| a == "--share");
    let out = std::path::PathBuf::from("/tmp/wren-notes-proof");
    std::fs::create_dir_all(&out).expect("create /tmp/wren-notes-proof");

    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(if share {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    });

    std::thread::spawn(move || {
        let mut failed = false;
        let mut check = |ok: bool, what: &str| {
            println!("{} {what}", if ok { "PASS" } else { "FAIL" });
            failed |= !ok;
        };

        // 1. Rich copy through the real command, then read the pasteboard back.
        let t = Instant::now();
        let copied = wren_lib::notes_export::notes_copy_rich(FRAGMENT.into(), PLAIN.into());
        println!("copy_rich: {copied:?} in {:?}", t.elapsed());
        check(copied.is_ok(), "copy_rich returned Ok");
        let pb = NSPasteboard::generalPasteboard();
        let types: Vec<String> =
            pb.types().map(|t| t.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        println!("pasteboard types: {types:?}");
        for want in ["public.html", "public.rtf", "public.utf8-plain-text"] {
            check(types.iter().any(|t| t == want), &format!("pasteboard has {want}"));
        }
        let plain = pb.stringForType(&NSString::from_str("public.utf8-plain-text")).map(|s| s.to_string());
        check(plain.as_deref() == Some(PLAIN), "plain text equals the input");
        for (ty, file) in [("public.html", "copy.html"), ("public.rtf", "copy.rtf")] {
            if let Some(d) = pb.dataForType(&NSString::from_str(ty)) {
                let bytes = d.to_vec();
                println!("{ty}: {} bytes → {}", bytes.len(), out.join(file).display());
                let _ = std::fs::write(out.join(file), bytes);
            }
        }

        // 2. Paginated PDF through the same code `write_pdf` runs.
        let pdf = out.join("notes.pdf");
        let t = Instant::now();
        let written = macos::write_pdf_blocking(&pdf_document(), &pdf);
        println!("write_pdf: {written:?} in {:?} → {}", t.elapsed(), pdf.display());
        check(written.is_ok(), "PDF written and starts with %PDF");

        // 2b. Two PDFs at once: AppKit allows one print operation at a time (a second one throws and
        // aborts), so whichever starts printing second must fail cleanly while the first succeeds.
        let both: Vec<_> = ["a", "b"]
            .map(|n| {
                let p = out.join(format!("concurrent-{n}.pdf"));
                std::thread::spawn(move || macos::write_pdf_blocking(&pdf_document(), &p).map_err(|e| e.to_string()))
            })
            .into_iter()
            .map(|h| h.join().expect("pdf thread"))
            .collect();
        println!("concurrent write_pdf: {both:?}");
        let busy = |r: &Result<(), String>| r.as_ref().is_err_and(|e| e.contains("still being saved"));
        check(
            both.iter().filter(|r| r.is_ok()).count() == 1 && both.iter().filter(|r| busy(r)).count() == 1,
            "two PDFs at once: one written, the other refused (no abort)",
        );

        // 3. Share picker anchored in a test window (top-right "button" at CSS [600, 16, 72, 28]).
        if share {
            DispatchQueue::main().exec_async(move || {
                let mtm = unsafe { MainThreadMarker::new_unchecked() };
                let window = unsafe {
                    NSWindow::initWithContentRect_styleMask_backing_defer(
                        mtm.alloc(),
                        NSRect::new(NSPoint::new(200.0, 300.0), NSSize::new(720.0, 420.0)),
                        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                        NSBackingStoreType::Buffered,
                        false,
                    )
                };
                unsafe { window.setReleasedWhenClosed(false) };
                window.setTitle(&NSString::from_str("Wren share proof"));
                window.makeKeyAndOrderFront(None);
                NSApplication::sharedApplication(mtm).activate();
                let view = window.contentView().expect("content view");
                let r = macos::show_share_picker(mtm, &view, FRAGMENT, PLAIN, "Q3 roadmap review – 25 Nov", [
                    600.0, 16.0, 72.0, 28.0,
                ]);
                println!("show_share_picker: {r:?} (content view flipped: {})", view.isFlipped());
                std::mem::forget(window);
            });
            std::thread::sleep(std::time::Duration::from_secs(3));
            let shot = out.join("share-picker.png");
            let s = std::process::Command::new("screencapture").arg("-x").arg(&shot).status();
            println!("screenshot: {s:?} → {}", shot.display());
        }

        std::process::exit(if failed { 1 } else { 0 });
    });

    app.run();
}
