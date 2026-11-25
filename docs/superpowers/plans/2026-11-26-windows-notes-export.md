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
