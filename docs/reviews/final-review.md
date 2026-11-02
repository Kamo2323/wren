# Wren — final whole-branch review

Reviewer: final review seat, 2 November 2026. Code: `main` at `3b9ba3a` (C:\Wren). Read-only: no code changed,
the app was not run, no keys injected.

## Verdict: **ready with fixes**

No Critical findings. Three Important findings and two coordinator-flagged deferred minors (FIX NOW) should
land in one wave before he relies on it. Every one is a small, local change with a unit test.

| | Count |
| --- | --- |
| Critical | 0 |
| Important | 3 |
| Minor | 8 |
| Deferred minors marked FIX NOW | 2 (+ Ruling 23, which is Important 3) |

Implement in this order: I-1, I-2, I-3, then the two FIX NOW minors (restore race, comma rule).

---

## Thread map (what was checked)

| Thread | Blocks on | Calls into other threads |
| --- | --- | --- |
| main (tao/WebView2) | sync commands only (none block: `get_settings`, `popup_discard`, `popup_hit_rect`, `engine_status`, `watcher_status`, `main_ready`, `popup_open_*`); `RunEvent::Exit` → `Services::shutdown` (≤ `QUIT_DRAIN_MAX_MS` + engine grace + clipboard flush) | session via channel |
| `wren-hotkey` (LL hook, HIGHEST) | nothing; callback never logs, never locks, always `CallNextHookEx` | dispatcher via unbounded channel |
| `wren-hotkey-dispatch` | `SendInput` of the mask | session via `try_send` (unbounded) |
| `wren-session` | `rec.start` (device open), `rec.stop` (writer join) | popup (posts only), hotkey (`PostMessage`), finisher (jobs channel) |
| `wren-finisher` | engine wait (≤ 90 s), transcribe (timeout), AI (≤ 2.5 s), paste (key-up ≤ 2 s, receipt ≤ 2 s, previous restore ≤ 0.5 s), pre-paste check (≤ 50 ms) | session via `Msg::PopupResult`, watcher start |
| engine supervisor / reader | child process, stdout | status channel → `wren-engine-status` → `emit` |
| audio writer / cpal callback | channel | level channel → `wren-levels` → `emit_to` |
| `wren-clipboard` (per paste) | own message loop | none |
| `wren-prepaste`, `wren-watcher` (COM MTA) | UIA with timeouts | session via `popup_result` (post) |
| `wren-popup-hover` | `GetCursorPos` poll | main via `run_on_main_thread` (post) |
| `wren-sweep` | store mutex, file deletes | none |

No lock is held across a call that waits on another thread; no thread waits on the main thread (popup
style changes are posted, the HWND is cached). Store calls serialise on one `parking_lot::Mutex<Connection>`;
the longest holder is delete-all / sweep (file deletes), which run on worker threads. Rulings 17, 26, 29
and 30 (hook.rs minors, session/popup concurrency, Task 9 r1, selftest/lib changes) are covered by this
review: nothing further found in those areas beyond the items below.

---

## Important

### I-1. Ctrl+V / Ctrl+Shift+V can be injected while he is holding the chord for the next dictation

**Where.** `src-tauri/src/paste/mod.rs:117` checks the chord keys once, before
`paste_via_clipboard` (`:130`). Between that check and `send(chord)` (`:211`) sit `Transaction::start`
(`paste/clipboard.rs:471-473` → `transaction_lock` → `flush_pending`, `:312-323`), which **waits for the
previous paste's restore — up to `CLIPBOARD_RESTORE_MS` (500 ms)** — and, for the retry, the full
`PASTE_READ_WAIT_MS` (1 s) receipt wait. The keys are never re-checked before either `SendInput`.
Separately, `session.rs:706-712` sets `hold_recording` only *after* `rec.start` returns (device open, tens
of ms; over a second on a cold or Bluetooth mic), so the finisher's guard at `session.rs:1109-1111` can miss
a hold that has just begun.

**Scenario.** Three quick dictations: job B (recorded while A was transcribing) reaches paste right after
A pasted, so B blocks ~500 ms on A's restore. He presses Ctrl+Win for C during that wait. B's Ctrl+V goes
out while Left Ctrl + Left Win are physically down → Windows sees **Win+Ctrl+V** (Windows 11: the sound-output
Quick Settings flyout, which takes focus). Worse, the injected `VK_CONTROL` *up* clears Left Ctrl in
`GetAsyncKeyState`; Win was pressed last, so Ctrl does not auto-repeat, and the next `TIMER_KEYSTATE` poll
(`hotkey/hook.rs:332-338`, every 250 ms) replays it as a lost key-up → `HoldEnd` (or `Tap`) — **C is cut
off mid-sentence** while he keeps talking. The same applies to the Ctrl+Shift+V retry 1 s after an unread
Ctrl+V. This is Review Focus #2 territory.

**Fix.**
1. Give `paste::paste_via_clipboard` a `keys_up: &dyn Fn() -> bool` and call it immediately before **each**
   `send(chord)` (Ctrl+V and the retry). `paste_text` passes `&wait_for_keys_up` (bounded by
   `KEYUP_WAIT_MAX_MS`). If it returns false: `tx.leave_text()` and return a new `Outcome::KeysHeld`, mapped
   to `PasteResult::KeysStillHeld` (already → `notPasted` + `copy_text` + "click to copy", shown after the
   recording ends via `deferred`).
2. In `SessionLoop::begin` set `hold_recording` (for `Mode::Hold`) **before** `rec.start(wav)`; clear it on
   the start-failure path (`session.rs:706-711`).
3. Tests: an ignored desktop test in `paste/clipboard.rs` style with `keys_up = || false` → `KeysHeld`, the
   `send` closure never called, text left on the clipboard; a session test with a fake recorder whose
   `start` sleeps, asserting the earlier job's `paste` is not called until that hold ends.

### I-2. Copying a card on Home/History puts dictated text into Windows clipboard history (and cloud clipboard)

**Where.** `src/lib/useTranscriptActions.ts:42-49` → `src/lib/api.ts:329-350` `copyText` uses
`navigator.clipboard.writeText` (fallback `execCommand("copy")`). That is an ordinary WebView2 copy with **no**
`ExcludeClipboardContentFromMonitorProcessing` / `CanIncludeInClipboardHistory=0` /
`CanUploadToCloudClipboard=0`. The Rust `paste::copy_text` (`paste/clipboard.rs:382-395`) does set them, and
its own doc says it is the one for "History's copy button" — the front end never calls it.

**Scenario.** He clicks a Home card (the primary "copy" gesture in spec §11). The text lands in Win+V history
(his machine has it on — the live test counted Win+V entries), where it outlives the 72 h retention, and, if
"sync across devices" is on, it is uploaded to Microsoft — against "nothing is sent anywhere".

**Fix.** In `api.ts` `copyText`: when `inTauri`, `await call("popup_copy_text", { text })` and return
true/false from it (consider renaming the command `copy_text`, keeping the old name as an alias for the
popup). Keep `navigator.clipboard` / textarea only for the browser mock. `copy_text` refuses empty text, so a
"not transcribed" card now says "Couldn't copy" instead of copying nothing — correct. Check: click a card,
Win+V shows no new entry.

### I-3. Ruling 23 was never applied: "mm" is still a filler, so "10 mm" becomes "10"

**Where.** `src-tauri/src/pipeline/tidy.rs:5` `FILLERS` still contains `"mm"` and an unconditional `"err"`;
the table test at `tidy.rs:157-158` enshrines it. Ruling 23 (in the final-fix-wave block of the ledger,
progress.md:105-107) says remove "mm"; remove "err" only in filler position.

**Scenario.** "Leave a 10 mm gap and a 5 mm edge." → pasted as "Leave a 10 gap and a 5 edge." Silent
corruption of measurements (his GardX/detailing dictation); "To err is human" loses a word the same way.

**Fix.** Drop `"mm"` from `FILLERS`. Treat `"err"` as a filler only when it is at a sentence start
(`at_sentence_start` in `remove_fillers`) or comma-adjacent (its trail contains `,` or `prev` ends with
`,`); otherwise keep the token. Tests: "Leave a 10 mm gap." unchanged; "To err is human." unchanged;
"Err, I think so." → "I think so."; "We could, err, try again." → "We could try again."; update the
all-fillers rows at `:157-158` (remove "mm"/"Mm"). Update SPEC §6's filler list to match the ruling.

---

## Minor (optional for this wave)

- **M-1. "No sound" self-dismisses while still recording.** `src/popup/Popup.tsx:170-173` arms the 4 s
  dismiss for every `noSound`, but the session also shows `NoSound` *during* a recording
  (`session.rs:769-773`, no hide time). With a muted mic the pill — including the hands-free timer and × —
  fades after 4 s while hands-free keeps recording up to 90 s. Fix: `NoSound { recording: bool }` and skip
  the timer/countdown when true (update the serde contract test and `types.ts`).
- **M-2. One deferred-notice slot.** `session.rs:446/677`: a second notice during one recording replaces the
  first (job A's "Not pasted · click to copy" lost to the watcher's "Learned" toast). The text is still in
  History. Fix if wanted: a small queue, or prefer `NotPasted` over `Learned`.
- **M-3. Failed engine shows "Warming up…".** `session.rs:714-715, 753-757, 852-856` treat any not-ready
  status as warming; with `EngineStatus::Failed` every dictation shows "Warming up…" then "Couldn't
  transcribe". Show `Error { "Speech engine stopped — open Wren" }` at chord-down when the status is `Failed`.
- **M-4. OOM retries on the same GPU state.** `session.rs:1008-1029`: an `Oom` (from
  `supervisor.rs:450-451`) is retried once on the unchanged, still-Ready engine, then saved without text.
  With Unity or another GPU app holding VRAM every dictation fails. Consider `engine.restart()` before the
  retry on `Oom`, and say "graphics card is full" in the error.
- **M-5. `engine_status` command ignores the engine swap.** `commands.rs:241-244` reads `engine().status()`;
  during `replace_engine` the UI can show the old engine's "Failed: shut down". Use `env.engine_status()`
  (`session/app.rs:116-118`).
- **M-6. Untranscribed rows count in stats.** `session.rs:939-957` inserts failed clips through
  `insert_transcription`, which adds a dictation and its duration (0 words) to `hourly_totals`
  (`store/mod.rs:457-466`): WPM drops, the streak counts a day with nothing transcribed.
- **M-7. Tray or popup creation errors are fatal.** `lib.rs:161-162` use `?` in `setup`, so a failure aborts
  start-up (`.expect` at `:255`; release is `panic = "abort"`). The popup helpers already no-op without a
  cached HWND; log and continue for both. Matters most if Start-with-Windows is switched on and the tray is
  not ready yet.
- **M-8. Uninstaller "Delete app data" does not delete Wren's data.** `installer/installer.nsi:924-939`
  removes `%APPDATA%\com.maximilian.wren` and `%LOCALAPPDATA%\com.maximilian.wren` (WebView2 data) only;
  `%LOCALAPPDATA%\Wren` (db, audio) and `%APPDATA%\Wren` stay. Safe by design (Ruling 27), but the label
  promises more. Reword the checkbox, or leave and mention it in the README.

## Not verified (worth one check this morning)

- A **real click** on the popup's ×, Undo and "click to copy" with the new hover tracking (Task 11 report says
  nobody pressed them). Check: during hands-free in the Claude app, click × — the recording is discarded **and
  the Claude composer keeps keyboard focus** (type a letter). If focus moves to the popup, that is a new
  Important (WebView2 child taking focus on click despite `WS_EX_NOACTIVATE`).

## Checked and fine

- Hook never swallows keys (`hook.rs:310-319` always `CallNextHookEx`); tagged events ignored; AltGr fake Ctrl
  filtered; reinstall installs-before-removes and a doubled callback is idempotent; watchdog wrap-safe.
- Only `VK_MASK` and Ctrl+V / Ctrl+Shift+V are ever injected, all tagged; no text typed key by key; final text
  can never contain a newline (`tidy::final_spacing` + test).
- Popup: HWND cached, show/hide async, style changes posted to the main thread, never a Tauri setter after
  creation; only the pill takes clicks; × / Undo / copy have `pointer-events: auto` and reach
  `popup_discard` / `popup_undo_learned` (`entryId` → `entry_id`) / `popup_copy_text`. App commands need no
  capability grant (no app manifest in `build.rs`).
- Contracts: `PopupState` serde (tag `kind`, camelCase fields, `handsfree`, `capWarning.mode`), `EngineStatus`
  (`state`), `WatcherStatus`, `AiCleanupStatus`, `Settings`, `UiConstants` match `src/lib/types.ts`.
- Privacy: every `tracing`/`println` checked — ids, counts, timings, process names only; `PopupState`,
  `TranscribeResult`, `Segment` Debug redacted; panic hook logs location only; sidecar logs fixed wording and
  exception type names; `log` records bridged at `info` carry no payloads; no external URLs in the front end;
  `HF_HUB_OFFLINE`; Ollama localhost only.
- Retention: sweep at start-up then hourly deletes audio then rows older than 72 h, orphan
  `correction_events`, orphan WAVs older than 1 h (recordings in progress keep a fresh mtime);
  `secure_delete=ON`; `wal_checkpoint(TRUNCATE)` after every delete; fixed-width timestamps compare correctly.
- Start-up resilience: corrupt settings → `.bad` + defaults; unopenable/too-new DB → unavailable store, paste
  still works; missing runtime/model → engine `Failed` with a reason; no mic / no hook → error popup.
- Uninstall never touches `%LOCALAPPDATA%\Wren`: install into it is refused, `RMDir "$INSTDIR"` is
  non-recursive, the data checkbox targets the bundle id folders only.
- Engine: Job Object + suspended launch; restart/back-off; generation guard on stale readers; timeouts on every
  wait; duplicate-id retry after a timeout is benign (same audio).

---

## Deferred minors triage (`final-review-inputs.md`)

| Item | Call | Reason |
| --- | --- | --- |
| T7: Fix-a-word Undo leaves the fix in stored `final_text` | LEAVE | His own correction; only visible after a reload; needs a new command. |
| T7: `dictionary::replace_whole_word` duplicates pipeline replace logic | LEAVE | Refactor with regression risk on the morning of first use; behaviour matches for normal words. |
| T5: restore race (open clipboard before the sequence/ownership check) | **FIX NOW** | `paste/clipboard.rs:682-698` checks `GetClipboardSequenceNumber`/`ownership_lost` *before* `OpenGuard::open` (which retries up to 100 ms); a copy he makes in that gap is overwritten by the restore. Open first, then check, then write, all under the one open. |
| T5: comma rule drops real commas after discourse markers | **FIX NOW** | `tidy.rs:83-86` pops the comma before any filler: "Yes, um, I agree." → "Yes I agree.". Keep it when the previous word (stripped, lower-case) is yes/no/so/well/okay/ok/right/now. Update `tidy.rs:160` to "So, I think we should go." and add the "Yes, um, I agree." row; `pipeline/mod.rs:158-160` stays as is ("great" is not a marker). |
| T5: reads before Ctrl+V counted as receipt | LEAVE | Exclusion formats keep monitors off; live Win+V check showed 0 new entries. |
| T5: paste core untested combos | LEAVE | I-1 adds the riskiest missing case (keys held before send). |
| T5: late worker overwrite after 3 s publish timeout | LEAVE | Needs another app to hang the clipboard for over 3 s. |
| T5: capitalisation edge cases (iPhone) | LEAVE | Only after a sentence-start filler; cosmetic. |
| T5: failed restore not retried | LEAVE | Only after five failed `OpenClipboard` attempts; rare, logged. |
| T5: no total backup cap | LEAVE | Per-format 256 MB cap already; transient memory only. |
| T5: exclusion-format failure only logged | LEAVE | `RegisterClipboardFormat` effectively never fails. |
| T5: null target guard | LEAVE | A paste with no foreground window is simply not read → "Copied". |
| T5: NotRead as string | LEAVE | Code smell, no behaviour change. |
| T5: ’ vs ' in replacements | LEAVE | Whisper and his typed entries both use straight apostrophes. |
| T5: Ctrl+Shift+V retry may double-paste on >1 s targets | LEAVE | Needs the target hung over 1 s; visible and not lossy. I-1's key check also guards the retry. |
| Ruling 23 ("mm", "err") | **FIX NOW** | Not implemented — see I-3. |
