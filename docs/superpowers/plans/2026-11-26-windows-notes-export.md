# Windows notes export — the Windows version of v0.1.12 (Mac) — brief for a fresh chat

Status: **not started.** Draft PR from branch `feat/windows-notes-export` (off `main` at 5d23145). Open this
file first, then `docs/HANDOFF.md` (top section) and the spec
`docs/superpowers/specs/2026-11-25-notes-export-design.md`.

## Goal

Ship to Windows what Mac users got in v0.1.11 + v0.1.12, so both apps match:

| Feature | Mac (shipped) | Windows today (on `main`, never released) | To do here |
| --- | --- | --- | --- |
| Drag-select enhanced notes | v0.1.11 | done (shared CSS) | live check only |
| Copy for email (formatted + plain) | v0.1.12 (HTML + RTF + plain) | done: CF_HTML + CF_UNICODETEXT, proven live in Edge | check in Outlook (classic + new) and Gmail in a browser |
| Save as Word (.docx) | v0.1.12 | done, proven end-to-end in the real app | — |
| Save as PDF | v0.1.12 (WKWebView print) | **hidden** (`notes_save_pdf` errors on non-mac) | **build** |
| Share | v0.1.12 (NSSharingServicePicker) | **hidden** | **decide + build** (see below) |
| Updater: "release without my platform = up to date" | v0.1.11 | done | — |
| Fix permissions button | macOS only | n/a on Windows (stays hidden) | — |

## Work

1. **Save as PDF on Windows** (`src-tauri/src/notes_export/mod.rs` → new `windows.rs`, mirroring `macos.rs`).
   Recommended: an off-screen/hidden WebView2 (a hidden Tauri `WebviewWindow`, or a WebView2 controller via the
   existing `webview2-com` 0.39 dependency) that loads `pdfDocument(...)` HTML and calls
   `ICoreWebView2_7::PrintToPdf(path, settings)` with A4/Letter by region and ~18 mm margins, background graphics
   on, header/footer off. Must be paginated, no app chrome, never shows a window, times out cleanly, and only one
   at a time (the Mac review's S3/S4 lessons). Then show the PDF button on Windows
   (`src/pages/meeting/EnhancedTab.tsx`, `isMac` gate) and update the page check that expects
   `Copy|Word|Regenerate` on Windows.
2. **Share on Windows — decide first (ask Maximilian).** Options:
   - A. Windows Share sheet (`IDataTransferManagerInterop::ShowShareUIForWindow` + `DataPackage` with HTML and
     text). Native, but on most PCs the targets are Teams/Outlook/Nearby Share and quality varies.
   - B. "Email" button instead: write a `.eml` draft (HTML body, subject = meeting title, `X-Unsent: 1`) to temp
     and open it, which opens a ready-to-send draft in Outlook classic/new Outlook/Thunderbird.
   - C. Leave Share Mac-only (Copy already covers email).
   Recommendation: B. It's the most useful for "send the notes by email" on Windows.
3. **Live proof on this PC** (the method from the Mac work, all scripts were in the session scratchpad and are
   easy to recreate):
   - Build the debug app (`npx tauri build --debug --no-bundle`), stop the installed Wren, run the debug exe
     with `WREN_HOME=<temp>`, `WREN_RUNTIME_ROOT=%LOCALAPPDATA%\Wren`, `WREN_TEST_AUDIO=...weather.wav`,
     `WREN_TEST_NO_HOTKEY=1`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9344`.
   - Seed one ended meeting + a medium note with Python sqlite3 into `<temp>\local\db\wren.db` (the meeting id
     **must be a UUID**, `meeting_get` rejects anything else).
   - Drive the page with Playwright `connectOverCDP`: open the meeting, click the Enhanced tab, click buttons.
   - The native Save dialog: UI Automation can't see its file-name box; focus the dialog
     (`SetForegroundWindow`) and `SendKeys` `^a`, the path, `{ENTER}`.
   - Restart the installed Wren afterwards.
   - For PDF: check pages/size (e.g. open with Python `pypdf` if installed, or check `%PDF` + render a page).
4. **Review loop** — Gemini 4 Neon at xhigh (GPT usage is unavailable), as for the Mac PR.
5. **Release Windows:** set the repo variable `WREN_RELEASE_PLATFORMS` back to `both` *before* merging
   (`gh variable set WREN_RELEASE_PLATFORMS --body both`). `WREN_SIGN_MACOS=self-signed` stays. The merge then
   publishes a release for both platforms. Mac users get a no-op-ish update with the same signature (fine).
   Check the release job's Mac log still shows `certificate root = H"fcac734a…"`.
   - GitHub sometimes can't get a macOS runner ("job was not acquired by Runner of type hosted"): just
     `gh run rerun <id> --failed`.

## Don'ts

- Don't change the shared `src/lib/notesExport.ts` output without re-running the unit tests (they unzip the .docx
  and lock the HTML/plain formats Mac users already rely on).
- Don't publish a Windows-only release by mistake: check `gh variable list` before merging.
- Copy on Windows is deliberately *not* excluded from clipboard history (unlike dictation).

## Build result (2026-11-26)

Built on `feat/windows-notes-export` (commits 652a5a8, 414cec9, 42d3d43, 76b3bc7). Items 1 and 2 above are done
and proven live on this PC. Items 4 (Gemini review) and 5 (release) are the lead's, not done here.

### What was built
- **Save as PDF on Windows** — `src-tauri/src/notes_export/windows.rs`. A hidden Tauri `WebviewWindow`
  (`visible(false)`, `focused(false)`, `skip_taskbar`, label `notes-pdf-<n>`) loads the page's `pdfDocument(...)`
  HTML from a temp file (`%TEMP%\Wren\pdf\notes-<n>.html`, deleted afterwards). When it finishes loading,
  `ICoreWebView2_7::PrintToPdf` prints it with WebView2 print settings: A4 (Letter when the regional format is
  US/CA, same rule as macOS), 18 mm margins, portrait, scale 1, backgrounds on, header/footer off.
  - One at a time: `PdfSlot` is taken *before* the save dialog and held until the print callback has really
    finished. A second request gets "Another PDF is still being saved. Try again in a moment."
  - Timeouts: 15 s to load, 30 s to print. A timeout before printing closes the window at once. A timeout
    *during* a print never tears it down (S4 lesson): the worker stops listening, the print callback cleans up,
    and if WebView2 never calls back, a reaper closes the window after 90 s and frees the slot.
- **Email on Windows (option B)** — `notes_email_draft` command + `notes_export/eml.rs`. Writes an unsent
  `.eml` (`X-Unsent: 1`, `MIME-Version`, `Date`, RFC 2047 UTF-8 subject = meeting title folded into ≤75-char
  encoded-words, `multipart/alternative` with text/plain + text/html, both UTF-8 base64 in 76-char lines, CRLF,
  boundary `=_WrenNotes_<uuid>` which can't occur in base64; no `To:`) to `%TEMP%\Wren\email-drafts\<id>\<title>.eml`
  and opens it with ShellExecute on the main (STA) thread. Before writing, `AssocQueryStringW` checks something
  handles `.eml`; if not, the person sees "No email app is set up to open drafts. Set one in Settings → Apps →
  Default apps (for .eml files), or use Copy and paste into an email." Draft folders older than 1 hour are
  swept each time. The page then shows "Opening a new email in <app>".
- **Page** — Windows toolbar is `Copy | Email | PDF | Word | Regenerate`; macOS unchanged
  (`Copy | Share | PDF | Word | Regenerate`). `src/lib/notesExport.ts` is untouched.

### Decisions and why
- Hidden Tauri window rather than a raw WebView2 controller: it reuses Wren's WebView2 environment and
  browser process (a second environment on the same user-data folder with different options fails), and Tauri
  owns the window lifetime. Its label matches no capability, so the page in it has no IPC permissions.
- Temp file instead of `NavigateToString`: NavigateToString caps pages at 2 MB (the command accepts 4 MB) and a
  file URL gives exactly one navigation to wait for.
- `PdfSlot` before the save dialog (not only at print time like macOS), so two requests can never race.
- Email draft has no recipient: the person picks one in their mail app.
- 1-hour draft sweep: mail apps read the file when they open it; keeping notes in temp longer has no use.
- `paper_for_region` is duplicated in windows.rs (inches) instead of shared with macos.rs, so macos.rs (which
  can't be compiled here) is untouched.

### Proof
- `cargo test --lib`: **456 passed, 0 failed, 16 ignored** (new: 4 eml tests, 6 windows tests).
- `cargo clippy --all-targets`: 20 lib / 24 lib-test warnings, all in files this branch doesn't touch
  (app_icons, updater, meetings, settings, …); none in `notes_export/`.
- `npm run build`: OK. `node scripts/unit-tests.mjs`: **37 passed, 0 failed**.
  `node scripts/check-pages.mjs`: **156 passed, 0 failed** (new: Windows toolbar, Email payload + toast,
  Windows PDF payload).
- Live, debug build (`WREN_BUILD_ID=winexport-1126b`, temp `WREN_HOME`, seeded UUID meeting + medium note with
  table, Unicode and 6 topics, Playwright over CDP, SendKeys into the native Save dialog):
  - PDF button → toast "Saved notes1.pdf" ~0.9 s after Enter. File starts `%PDF-`, **3 pages, all
    595.0 × 841.9 pt (A4)**, text starts 18.0 mm from the left and 17.8 mm from the top, text contains
    Kraków / Résumé / 李雷 / Owner. Page renders: `docs/screenshots/windows-notes-export/pdf-page1.png`,
    `pdf-page2.png` — tinted "In short" box and table header printed, no header/footer, no app chrome.
  - The `Wren PDF` window was seen 9 times by a 15 ms window scan during a print, never visible; afterwards CDP
    lists only the main page and popup, and `%TEMP%\Wren\pdf` is empty.
  - Two at once: PDF #1 started (save dialog open) → PDF #2 returned "Another PDF is still being saved. Try
    again in a moment." → #1 saved → #3 from the button saved fine (slot released).
  - Email → `.eml` written (25,000 bytes, ASCII-only, CRLF-only); Python's `email` parser reads Subject
    "Weekly sync with Sarah — Q3 “plan” & Zoë", `X-Unsent: 1`, text/plain + text/html utf-8 base64, no defects.
    Handler on this PC: **new Outlook** ("Outlook", `olk.exe`), which launched — but it stopped at "You need to
    sign in" for the mail account, so the compose window itself could not be seen (not signed in on his
    behalf; that screenshot shows a personal address and was not committed). Toast "Opening a new email in
    Outlook" seen live. Wren stayed responsive (PDF afterwards saved fine).
  - Toolbar: `docs/screenshots/windows-notes-export/enhanced-toolbar.png`.

### Not done / unproven
- **Does new Outlook open an `X-Unsent` .eml as an editable draft or as a read-only message?** Unproven here
  (account needs sign-in). Outlook classic honours `X-Unsent: 1`; Thunderbird opens .eml read-only (Edit as
  New). Worth Maximilian clicking Email once with Outlook signed in.
- "No mail app" path: proven only at the unit level (`file_handler(".wren-no-such-type-…") == None`), not by
  removing the association on this PC.
- Copy in Outlook (classic/new) and Gmail — not re-checked in this round.
- PDF of a Letter-region PC not run live (unit test for the region rule only).
- macOS build not compiled here (macos.rs untouched; mod.rs cfg blocks reviewed by hand).
- Review loop (item 4) and release (item 5) left to the lead.
