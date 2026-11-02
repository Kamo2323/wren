# Claude desktop app: UI Automation probe (read-only)

Run 2 November 2026 with `cargo run --example uia_probe` (source: `src-tauri/examples/uia_probe.rs`).
This decides whether Wren's correction watcher (spec §8) is on by default.

**Verdict: readable.** The Claude desktop app's message box can be read in well under 50 ms (p95 0 ms,
max 1 ms over 20 reads). The watcher is therefore **on by default** (`watcher_enabled = None` means on).

## How it was run

- Read-only throughout. The probe never focuses the app, never sends input and never sets a value: it
  asks UI Automation for the window's element tree and reads text, nothing else. The build session
  itself was running inside the Claude app at the time.
- No text from the app is recorded here or in the probe's output — only lengths, flags and timings.
- Machine: Windows 11 Home 26200, RTX 3060 Ti. Claude app pid 32212, one visible window.

## What is readable

| | Result |
|---|---|
| UI Automation client created | 6–8 ms |
| Search for editable elements in the window (one `FindAll`, descendants) | 145–171 ms (one-off; the watcher never does this) |
| Elements that are an Edit control or have a writable value | 9 |
| The message box | `Edit`, framework `Chrome`, class `tiptap ProseMirror ProseMirror-focused` |
| Its patterns | ValuePattern (writable), TextPattern, TextEditPattern, keyboard-focusable |
| Was it the focused element during the probe | yes |
| 20 reads of the message box (fresh facts + `TextPattern.DocumentRange.GetText(-1)`) | **p50 0 ms, p95 0 ms, max 1 ms**, 0 failures, 0 slow (> 250 ms) |
| First read came back empty | no (the box itself was empty at the time; see below) |
| Empty message box reads as | one whitespace character (a line break) on every read |
| Watcher-style poll of the focused element (`GetFocusedElement` + text), 20× | p50 1 ms, p95 2 ms, max 2 ms; focus in the Claude app 20/20 |

Other things seen in the same window: a second ProseMirror box (another session's composer,
not focused, also reads as blank), two plain Edit boxes (42 and 46 characters: title fields) and three
segmented-control elements with a writable value but no text.

## Things the watcher relies on, confirmed here

- The composer exposes **TextPattern**, so the watcher reads it with `DocumentRange.GetText(-1)`;
  ValuePattern is there as the fallback.
- An **empty composer reads as a single line break**, not as an empty string. The watcher's
  "the box emptied (message sent)" rule therefore treats whitespace-only text as empty (`track::is_blank`).
- Chromium reports **TextEditPattern and a read-only ValuePattern on nearly every node** (1,657
  elements in this window matched "Edit, Document or TextEdit"). So "TextEditPattern available" says
  nothing about whether something is editable. The pre-paste check (ruling 24) uses the writable
  ValuePattern and the control type instead.
- Not measured: a long draft in the composer (it was empty during the probe). The same window's
  conversation document (about 112,000 characters) read in p50 50 ms / p95 83 ms in the first run,
  so even a very long composer would stay far below the 250 ms slow-call line.

## Codex desktop app

Not running at the time of the probe (only its background sandbox service was), so it was not probed.
Run `cargo run --example uia_probe` again with Codex open to check it.
