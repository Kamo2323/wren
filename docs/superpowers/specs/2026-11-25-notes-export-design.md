# Enhanced notes: select, copy for email, export, share — and Mac permissions that survive updates

Date: 2026-11-25 · Branch: `feat/notes-export` (off `main` at 005ceff, which already contains Meetings / v0.1.9)

## Why

Maximilian ends a Teams meeting, gets Claude's enhanced notes in Wren, and wants to send them on — mostly by
email. Today:

- He can't drag-select the notes: `body { user-select: none }` (`src/styles/base.css:24`) and `.md` never
  turns selection back on.
- The only way out is **Copy**, which puts the raw Markdown on the clipboard, so a pasted email shows `**`,
  `#` and pipe tables.
- There's no export at all.

Separately, after the v0.1.9 update his dad's Mac said Wren had no Accessibility / audio permissions even though
the switches in System Settings were on. Cause (verified in the repo): no Apple signing secrets are set, so CI
ships an **ad-hoc signed** app. macOS ties privacy permissions (TCC) to the app's signature; an ad-hoc signature
is a hash of that exact build, so every update looks like a new app and the old "on" switches no longer apply.

Most meetings happen on the Mac, so **this ships Mac-only first**; Windows gets a later update.

## Goals

1. Enhanced notes text can be drag-selected and copied (⌘C) like any document.
2. **Copy** puts formatted notes on the clipboard: pasting into Apple Mail, Outlook (Mac) or Gmail keeps headings,
   bold, bullets, checkboxes and the action table — no Markdown symbols. Pasting into a plain-text field gives clean
   readable text (no `**`, `#`, `|`).
3. **Save as PDF** — one click → Save dialog → a paginated PDF that looks like the notes in the app.
4. **Save as Word** — one click → Save dialog → a real `.docx` with proper headings, lists and table.
5. **Share** (Mac) — the standard macOS Share menu: Mail (formatted message, meeting title as subject), Notes,
   AirDrop, Messages, etc.
6. Mac privacy permissions survive future updates (free self-signed certificate, option A).
7. A hint in Wren for anyone hitting the stale-permission glitch.
8. A Mac-only release that doesn't disturb Windows users on v0.1.9.

## Non-goals

- Copy as Markdown and Save as `.md` (offered, not chosen).
- Windows Share menu; Windows native pieces of this feature (see "Windows").
- Apple Developer ID / notarisation (option B, declined for now).
- Exporting the transcript or My notes (only enhanced notes were asked for).
- Fixing the open mute-gap draft PR #2.

## Anti-goals (would be a failure even if it "works")

- Pasted email shows any Markdown syntax, or the action table collapses into one line in Outlook.
- PDF that's one endless page, cuts lines in half across pages, or shows app chrome/buttons.
- Windows users on v0.1.9 get a broken or looping update because of the Mac-only release.
- Any export path that can run script or load remote resources from the model's output (keep the existing
  DOMPurify allow-list as the single source of rendered HTML).
- A regression in dictation paste (the clipboard code is shared with paste transactions).

## Approaches considered

**Email formatting**
- *Chosen:* Copy writes several flavours at once — HTML (with inline styles so Outlook keeps the table), RTF
  (made from that HTML by AppKit, for Pages/TextEdit and apps that prefer RTF) and clean plain text.
- Rejected: a separate "Copy for email" button (two copy buttons confuse; formatted is right everywhere).
- Rejected: `mailto:` links — can't carry HTML bodies.

**PDF**
- *Chosen:* render the sanitised notes HTML in an off-screen WKWebView with a print stylesheet and save through
  AppKit's print operation straight to a file (no print panel) → paginated, matches on-screen look.
- Rejected: macOS print panel ("Save as PDF" from it) — extra clicks, prints app chrome.
- Rejected: `WKWebView.createPDF` alone — produces one tall page, no pagination.
- Rejected: drawing the PDF ourselves in Rust — would never match the app's look.

**Word**
- *Chosen:* build the `.docx` from the same Markdown tokens the app already parses (`marked`), in the frontend with
  the `docx` npm library; Rust only writes the bytes to the path the Save dialog returned. Builder may instead do it
  in Rust (`pulldown-cmark` + a docx crate) if that proves cleaner — output requirements are what matter.
- Rejected: HTML saved with a `.doc` extension — opens with warnings, not a real Word file.

**Share**
- *Chosen:* `NSSharingServicePicker` anchored at the Share button, sharing an attributed string built from the notes
  HTML (Mail gets a formatted body) with the meeting title as subject.

**Permissions** — *chosen by Maximilian:* A, a free self-signed code-signing certificate stored in GitHub secrets;
every Mac build is signed with it, so the signature's designated requirement (identifier + certificate) is stable
and TCC grants carry over. Rejected: B, Developer ID ($99/yr) — later, if Wren goes wider.

**Release** — *chosen by Maximilian:* B, Mac-only release.

## Design

### 1. Selection (`src/styles/meetings.css`)
`.md` (enhanced notes body) gets `user-select: text; -webkit-user-select: text; cursor: text`. Links stay
clickable. Builder's call: also apply to transcript line text if it's equally unselectable (cheap, same reason).

### 2. Notes toolbar (`EnhancedTab.tsx`, the existing `.enh-bar`)
`Copy` · `Share` (macOS only) · `Export ▾` (Save as PDF, Save as Word) · `Regenerate`. Toasts: "Notes copied",
"Saved <filename>", or a plain error. Default filename: `<meeting title> – <date> (<Length> notes).pdf|docx`,
with characters illegal in filenames removed. Export is available for a finished note only (not while streaming).

### 3. One HTML builder for all outputs (frontend)
A single function turns a note into a self-contained "email/export HTML" document: the sanitised notes HTML
(existing `toHtml`) plus a title line (meeting title, date, length) and **inline styles** (system font, sensible
sizes, bordered table cells, checkbox glyphs ☐/☑ instead of `<input>`). Same function feeds Copy (HTML flavour),
PDF, Share, and Word (or Word uses the Markdown tokens — builder's call). A second function produces the clean
plain-text version: headings as their text, bullets as "• ", numbered lists kept, checkboxes as "☐ / ☑", tables as
"Column: value" lines or aligned columns, links as "text (url)".

### 4. Native (Rust, macOS) — new Tauri commands
- `copy_rich(html, plain)` → `clipboard_macos.rs`: one pasteboard write with `public.html`, `public.rtf`
  (via `NSAttributedString` from HTML, on the main thread) and `public.utf8-plain-text`; uses the existing
  `transaction_lock()` so it can't race a dictation paste. Old `copy_text` stays for History etc.
- `export_notes_pdf(html, suggested_name)` → Save panel; off-screen WKWebView loads the HTML; print operation with
  A4/Letter by locale, margins ~18 mm, `showsPrintPanel = false`, job disposition "save" to the chosen URL.
  Print CSS: no breaks inside table rows / list items / headings kept with next paragraph.
- `save_bytes(bytes, suggested_name, kind)` → Save panel → write file (for Word). Use `tauri-plugin-dialog`
  (new dependency) or AppKit `NSSavePanel` directly — builder's call; must be a native sheet/panel.
- `share_notes(html, subject, anchor_rect)` → `NSSharingServicePicker` with the attributed string; subject set on
  the Mail service.
- Cancel in any Save panel = silent no-op (no error toast).

### 5. Permission hint
Where Wren shows a missing Accessibility / Microphone / Screen & System Audio Recording permission on macOS
(Setup page and any Meetings/Settings banners), add: "Already switched on? After an update macOS can lose track
of it — select Wren, press −, then add it again." The "Open Settings" button stays.

### 6. Stable Mac signing (CI)
- Create a self-signed code-signing certificate (Common Name e.g. "Wren Self-Signed", EKU codeSigning, 10-year
  validity) once; store as secrets `MAC_SELF_SIGNED_P12` + `MAC_SELF_SIGNED_P12_PASSWORD`. Keep a copy of the
  `.p12` + password outside the repo — losing it means one more permission reset for every Mac user.
- `release.yml` macOS job: when Developer ID isn't configured but the self-signed secrets are, import into a temp
  keychain, trust it for code signing on the runner, and build with that identity (hardened runtime + existing
  entitlements, no notarisation). Fail the release if neither is present rather than silently shipping ad-hoc.
- Verify in CI: `codesign -dr - Wren.app` shows a designated requirement naming the certificate, not a cdhash.
- The updater's `.app.tar.gz` staging must keep the signature intact (verify by updating a self-signed build to
  another self-signed build on the MacBook and confirming permissions persist).

### 7. Mac-only release
- New repo variable `WREN_RELEASE_PLATFORMS` (`both` default, `mac`, `windows`). The workflow skips the Windows
  build when it's `mac`; `ci-release-meta.mjs` already accepts a Mac-only manifest.
- Effect on Windows v0.1.9 (verified in `updater.rs`): background checks see no `windows-x86_64` entry → error →
  silently ignored, no Update pill. A manual check shows "This release doesn't include a Windows installer." —
  acceptable for now; change Windows builds to treat a missing platform as "up to date" so future Mac-only
  releases are silent once Windows has this code.
- Set the variable to `mac` before merging; set it back to `both` for the Windows update.

### Windows (this branch)
Windows must still build and pass its checks. Share is hidden on Windows. Copy/PDF/Word on Windows: implement if
cheap (CF_HTML + CF_UNICODETEXT; WebView2 `PrintToPdf`; same save dialog), otherwise hide those buttons on Windows
and leave them to the Windows update. Not released either way in this round.

## Testing
- Unit: plain-text conversion and HTML builder (headings, nested lists, checkboxes, table, links, characters that
  need escaping, a note with HTML injected by the model is still stripped); filename sanitising; manifest/platform
  gating; Rust tests for any pure helpers.
- Page checks (existing Playwright suite): notes text selectable; toolbar buttons present per platform.
- **Live on the MacBook (T3 thread):** drag-select + ⌘C; Copy → paste into Apple Mail, Outlook for Mac, Gmail in
  Safari (screenshots); Save as PDF and open in Preview (pagination, no chrome); Save as Word and open in Word or
  Pages; Share → Mail shows a formatted draft with subject; cancel paths; dictation paste still works.
- Signing: install self-signed build N, grant permissions, update to self-signed build N+1 through the in-app
  updater, confirm permissions still work without re-granting.

## Release steps
1. Merge to `main` with `WREN_RELEASE_PLATFORMS=mac` → CI publishes a Mac-only release.
2. On both Macs: update, then one last permission reset (the signature changes from ad-hoc to the certificate
   once). Later updates keep permissions.

## Builder-check answers (2026-11-25)

- **Updater re-sign (critical).** The macOS install helper in `updater.rs` re-signs any non-Developer-ID update
  with a local "Apple Development"/"Developer ID" identity if one exists. That's why Maximilian's MacBook (Xcode)
  kept permissions and his dad's Mac (no identities) didn't. New rule: an update signed by Wren's self-signed
  certificate is kept exactly as shipped, like Developer ID. Note the *installed* v0.1.9 helper does the install of
  the first self-signed release, so on a Mac with a dev identity that one update may still be re-signed locally;
  from the next one on, the new helper keeps the shipped signature. (Maximilian's call: priority order — this
  permissions fix ships first as its own Mac-only release; the notes features follow in a second release.)
- **N → N+1 test.** No throwaway public release: test on the MacBook with two locally built, self-signed builds
  and a local update source / the install helper directly.
- **Certificate.** Claude generates it with openssl, sets the secrets with `gh` (account owns `Kamo2323/wren`),
  and keeps the `.p12` + password backup outside the repo in `%USERPROFILE%\Documents\Wren signing\`.
  Maximilian should copy it into his password manager.
- **Forks.** Hard failure only when the repo opts in: `WREN_SIGN_MACOS=self-signed` requires the self-signed
  secrets; unset keeps today's ad-hoc behaviour with a warning (forks unaffected). `true` stays Developer ID.
- **Self-signed build signing.** Mirrors today's ad-hoc path (`codesign --force --deep --sign <identity>`, no
  Hardened Runtime, no notarisation) so runtime behaviour is unchanged apart from the stable identity.

## Left to the builder
docx generation side (JS vs Rust), dialog implementation, exact print CSS, whether transcript text also becomes
selectable, and whether Windows gets the cheap native pieces now or buttons are hidden.
