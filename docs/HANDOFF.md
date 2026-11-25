# Wren — handoff and decision log

## Status (2026-11-25) — Mac update: permissions survive updates; notes select/copy/export (branch `feat/notes-export`)

Spec: `docs/superpowers/specs/2026-11-25-notes-export-design.md` (builder-checked; notes part not yet approved
by Maximilian). Maximilian's priorities: **Mac first**, **Mac-only release** (Windows update later), and the
permissions bug first because it stops his dad using Wren at all.

### The permissions bug (dad's Mac after v0.1.9: "needs Accessibility / audio recording" though both are on)
- Cause: no Apple signing secrets, so CI shipped **ad-hoc** signed apps. TCC ties permissions to the signature's
  designated requirement; ad-hoc = this build's cdhash, so each update is a "new app". Toggling the switch doesn't
  help (it belongs to the old signature); `tccutil reset` or remove (−) + re-add does.
- Maximilian's MacBook was unaffected because the old update helper re-signs non-Developer-ID updates with a local
  "Apple Development" identity (it has one; dad's Mac has none).
- Fix (option A, Maximilian's choice; B = $99 Developer ID declined for now):
  - Self-signed cert "Wren Self-Signed Code Signing" (20 yr). Backup: `%USERPROFILE%\Documents\Wren signing\`
    (.p12 + password + README). Secrets `MAC_SELF_SIGNED_P12`, `MAC_SELF_SIGNED_P12_PASSWORD` set on Kamo2323/wren.
  - CI `WREN_SIGN_MACOS=self-signed` (variable set) signs like the ad-hoc path with that identity and fails unless
    the designated requirement names the certificate. Untrusted cert is fine; its keychain must be on the search
    list (`--keychain` alone fails — tested on macOS 26.4).
  - Updater helper keeps builds signed by that cert as shipped (`MAC_RELEASE_SIGNING_IDENTITY`).
  - Updater: a release without this platform's entry = "up to date" (was an error).
  - **Fix permissions** button (banner, Setup, Meetings card, Settings row) → `tccutil reset Accessibility|
    Microphone|AudioCapture com.maximilian.wren` (no password needed) then the Accessibility prompt.
  - CI `WREN_RELEASE_PLATFORMS=mac` (variable set) + `dry_run` input. Docs: `docs/RELEASING.md`.
- Also done: enhanced notes are drag-selectable (`.md { user-select: text }`) + page check (fails without the fix).

### Verified
- Windows: Rust lib 443 passed / 0 failed, 0 build warnings; JS unit tests passed; page checks all passed.
- MacBook (T3 child): 0 warnings, lib 498 passed; helper test with dummy bundles: self-signed update logged
  "Keeping the published Wren release signature" and kept its Authority; ad-hoc control re-signed with Apple
  Development; designated requirement identical across rebuilds (certificate leaf, no cdhash); `tccutil reset`
  for an app in ~/Applications exit 0, no password; screenshots fine (hint since shortened).
- CI dry run 45628440589 (branch, Mac only, nothing published): success. `designated => identifier "com.maximilian.wren" and certificate root = H"fcac734a…"`; update signature verified with the app key.

### Not done / next
1. Merge `feat/notes-export` → `main` for the **permissions release** (Mac-only), once the dry run is green.
   After updating, each Mac needs **one** last reset (signature changes from ad-hoc/Apple Development to the
   cert): press Fix permissions and allow. On Maximilian's Mac the v0.1.9 helper may re-sign the first
   self-signed update with Apple Development, so his one reset may come one update later.
2. Notes features (needs Maximilian's approval of the spec): formatted Copy (HTML+RTF+plain), Save as PDF,
   Save as Word, Mac Share menu. Then writing-plans → build → live tests on the MacBook → Mac-only release.
3. Before the next Windows release: set `WREN_RELEASE_PLATFORMS=both`; Windows pieces of the notes work.

## Status (2026-11-25) — Public readiness: in-app speech setup, portability, updater, signing

Branch `t3/speech-onboarding-portability`, based on `origin/main` 09c8cfd (the two unrelated Teams
commits fb41104/d3988fb were dropped from this branch). Not merged, no PR yet (parent agent files it).
Teams/Meetings capture, transcripts and Claude notes are untouched.

### Completed
- **First-run speech setup in the app** (`src-tauri/src/runtime_setup/`, `src/pages/Setup.tsx`): pinned
  manifest `sidecar/runtime-manifest.json` (uv 0.12.13 + SHA-256, Python 3.12, every model file with
  size + SHA-256). Downloads resume with HTTP Range; venv built in `runtime/.venv.staging` and swapped
  in only after `import` works; Wren-made model folders replaced, others moved aside; `setup-cache/`
  removed on success (uv's package cache freed right after the swap). Engine waits in a new
  `NeedsSetup` state instead of crash-looping; setup is Done only when the real engine says Ready.
  UI: sizes/free space, step progress, Pause/Continue, errors + Try again, ready check (mic meter,
  macOS Accessibility row, try-it box). Settings → Speech engine → Repair speech setup.
- **Profile portability**: default name is now empty (meeting lines say "Me", Home says "Hi there.");
  a saved `meetingName` is kept (Maximilian's settings.json has it saved). Settings label "Your name".
- **Build portability**: release/live-test default to `<checkout>\.cargo-target`, `.release-out`,
  `.live-test` (git-ignored); `release.sh` → `.release-out-macos`. No owner paths in script defaults.
- **Updater**: anonymous by default; token/`gh` fallback only with `WREN_UPDATE_PRIVATE=1` (build or run
  time). Build-time `WREN_UPDATE_REPO=owner/repo|none` + `WREN_UPDATE_PUBKEY` for forks. macOS update
  helper no longer re-signs a Developer ID build.
- **Release workflow**: `WREN_SIGN_WINDOWS` / `WREN_SIGN_MACOS` variables request Authenticode or
  Developer ID + notarization; fail closed (missing secrets, failed verify, publish-time proof file).
  Details: `docs/RELEASING.md`. `scripts/sign-windows.ps1` (import / Tauri overlay / verify).
- Test seam `WREN_TEST_NO_HOTKEY=1` (like `WREN_TEST_AUDIO`).
- Windows: WebView2 microphone permission auto-allowed for Wren's own page only (fresh profiles got
  a "tauri.localhost wants to use your microphones" bubble).

### Proof (raw numbers; screenshots and logs in `C:\Users\maxha\.t3\proof\wren-speech-onboarding\`)
- `cargo test --lib`: 431 passed, 0 failed, 15 ignored; 0 build warnings. `node scripts/unit-tests.mjs`:
  30 PASS. `npm run build` ok. `node scripts/check-pages.mjs`: 144 PASS. `nsis-template.mjs --check` ok.
- Real app, empty profile: offline error → online setup (uv, Python, packages in ~3 min, models) →
  pause → continue → hard kill mid-download → relaunch resumes → warm-up → Ready.
  `wren --selftest` on that runtime: SELFTEST PASS, large-v3-turbo on CUDA, warm-up clip 277 ms.
- Existing runtime (read-only via `WREN_RUNTIME_ROOT`): no setup shown, engine Ready, files unchanged.
- Installer payload: wren.exe + `_up_/sidecar` (py, both uv.lock files, warmup.wav), no Python/models.
- Authenticode wiring with a throwaway self-signed cert: installer, packaged wren.exe, uninstaller and
  NSIS plugins signed + timestamped; `-RequireTrusted` and the unsigned build correctly fail.

### Found by the real run and fixed (6178cca, a5306e5)
- Pause after the runtime swap left the engine "Speech stopped" → now restarts into needs-setup.
- Kill during the backup-model download reported setup finished → stays open until complete.
- WebView2 mic prompt blocked the ready check (and the Settings mic test on fresh installs).
- CI verify pointed at `target\release\wren.exe` (unsigned leftover) → now verifies the payload.

### NOT done / needs Maximilian or other hardware
- **macOS not run** (no Mac here): setup flow (MLX, `/usr/bin/tar`, statvfs), mac UI rows, the
  `macos_apply_script` change (syntax-checked with `sh -n` only), Developer ID/notarization steps.
- **No trusted publisher certificate / Apple Developer account**: real Authenticode `-RequireTrusted`
  and notarization are unproven; the workflow has never run with signing on.
- **Chord dictate → paste not run live**: his Wren was running and both would react to the chord.
  Engine transcription was proven by the self-test. To finish: quit Wren, then
  `scripts\live-test.ps1 -Wren <proof exe>` with the proof profile (`WREN_RUNTIME_ROOT`).
- Resume offset (Range) is proven by unit tests; the live log doesn't record byte offsets.
- **Decision for Maximilian**: while the repo is private, his installs no longer update anonymously.
  Set repo variable `WREN_UPDATE_PRIVATE=1` (CI builds keep the gh fallback) or user env
  `WREN_UPDATE_PRIVATE=1` on his PCs; remove before going public.
- App identifier `com.maximilian.wren` kept on purpose (changing it breaks upgrades, WebView2 data
  and macOS permissions). Sidecar pytest not run (no sidecar Python changed).

### Review round 1 (Sol) — disposition (commits eac58fc, 2eb6250)
- **S1 fixed.** Engine status events carry the engine's generation (`session::app::tagged_status`);
  stale engines' events reach neither the UI nor setup; setup completes on the Ready of the engine it
  started (set under the status lock, so no race). Test `a_no_change_repair_waits_for_the_new_engine…`;
  live: no-change repair on a complete profile → Done (log: gen 0 "shut down" `current:false`, gen 1 Ready).
- **S2 fixed.** `setup-repair-pending` marker written before anything changes, cleared only on the new
  engine's Ready; a pause, plain retry (`repair=false`) or relaunch repairs again. Tests
  `repair_intent_survives_pause_retry_and_relaunch…`, `a_readable_but_corrupt_model_fails_the_repair_check`.
  Live: same-size zeroed `tokenizer.json` → repair → app killed → relaunch reopens setup ("A speech
  repair didn't finish") → plain retry ran `repair: true` → only that file re-fetched, pinned hash restored.
- **S3 fixed.** Plan, preflight (`space_needed`, one function for both) and progress count only what is
  left (`.part` + staged files). Percent labelled "overall", detail "models: x of y". Test
  `a_partial_download_only_counts_what_is_left`; live: after pause, main model 1.1 GB left (532 MB kept),
  needs 1.9 GB instead of 2.4 GB.
- **S4 fixed.** `examples/verify_update.rs` runs the app's `verify_signature` with the key built into the
  same build; both release jobs run it after signing; update-signing exit codes checked. Local: official
  key vs throwaway-signed file → FAIL; matching fork key → OK; wrong version → FAIL.
- **S5 fixed.** uv only runs when unpacked this run from the re-hashed archive; cleanup only deletes files.
  Live: a planted fake `uv.exe` in `setup-cache` was replaced, never run.
- **Self-test CPU fallback fixed.** Passes when the clip transcribes, names the device. Live:
  CUDA → `large-v3-turbo ready on cuda … 272 ms` PASS; `CUDA_VISIBLE_DEVICES=-1` + unreachable proxy →
  `small.en ready on cpu (processor fallback, no CUDA: slower) … 1547 ms` PASS.
- **Found in the live proofs and fixed (2eb6250):** a reopened setup didn't say why and offered "Download
  about 1 MB"; now it shows the reason and labels the button Check and repair / Continue setup.
- A real network drop happened during the round-2 run: the clear "connection dropped… try again" error,
  partial kept, retry finished the download.
- Checks at eac58fc: Rust lib 435 passed, 0 failed, 15 ignored, 0 warnings (lib + examples); 30 JS unit
  tests, 144 page checks, frontend build OK; workflow YAML valid. 2eb6250 (UI only): tsc + 144 page checks.
- Round-2 screenshots: `C:\Users\maxha\.t3\proof\wren-speech-onboarding\screenshots\r2-01-first-run-setup-screen.png`,
  `r2-02-progress-packages-overall.png`, `r2-03-progress-models-overall.png`,
  `r2-04-resume-counts-only-what-is-left.png`, `12-repair-reopens-after-crash.png` (before the 2eb6250 wording).
  Self-test logs: `selftest-cuda.txt`, `selftest-cpu-fallback.txt` in the same folder.

### Review round 2 — disposition (commit afc923b; S4/S5 handled by the parent in its own worktree)
- **S3 (staged files) fixed.** `download::fetch` deletes a finished staged file that fails its pinned
  checksum or size before downloading its replacement (never installed models: only the setup cache).
  `staged_bytes` credits exact-size finished files and `.part` files no bigger than the file; wrong-size
  finished files and oversized `.part` files count nothing. Tests:
  `a_damaged_finished_file_is_removed_before_its_replacement_downloads` (full-size corrupt, oversized,
  short; fails with the fix disabled), `a_damaged_finished_file_next_to_a_partial_one_resumes_the_partial`
  (Range resume kept), `staged_bytes_count_only_what_a_resume_keeps`.
- **R2-1 (engine swap order) fixed.** `EngineSwap` takes the generation inside the swap turn, so
  generation order is install order and the installed engine is always current. `replace_engine`
  returns a floor; setup completes on the first Ready of any engine installed after it (settings saves
  and manual restarts converge; no gating needed). Callers reviewed: `save_settings` (floor ignored),
  setup's `installed` hook (floor awaited), `engine_restart` (same engine, same generation). Tests:
  `generations_follow_install_order_under_overlapping_replacements` (8 overlapping swaps),
  `a_settings_change_that_overtakes_setups_engine_still_completes_setup`. Live: repair → setup's engine
  (gen 1) overtaken by a beam-size save (gen 2) → gen 1 events `current:false` → Done on gen 2's Ready
  in 6 s; setting restored (gen 3 Ready). Script: `C:\Users\maxha\.t3\proof\wren-speech-onboarding\tools\overlap.mjs`.
- Checks at afc923b: Rust lib 440 passed, 0 failed, 15 ignored; 0 warnings (lib + examples); JS unit
  tests pass. One run saw `meetings::session::tests::backlog_is_capped_and_marked_not_transcribed` fail
  (pre-existing fixed 300 ms sleep under load); 5/5 alone and the next full run passed. Not touched.

### Remaining gaps (exact)
- macOS: nothing run here (parent has a Mac for independent proof).
- Trusted Authenticode / Developer ID / notarization: no credentials; never run with signing on.
- The CI `verify_update` step was proven locally in debug only, not in Actions.
- Chord dictate → paste not run live (his Wren running); engine transcription proven by self-test.
- No live low-disk test; the space check for a resume is unit-tested.

### Next step
Parent: independent review, then PR from `t3/speech-onboarding-portability` to `main`.

## Status (2026-11-24, building) — Meetings feature on branch `feat/meetings`

- Plan: `docs/superpowers/plans/2026-11-24-meetings.md` (native execution). Ledger with every ruling:
  `.superpowers/sdd/2026-11-24-meetings/progress.md` (git-ignored) — read it first when resuming.
- Done: Tasks 1–5, 7, 8 (storage/retention, segmenter, speaker attribution, Teams UIA reader + probe,
  Windows Teams process-loopback capture via the `wasapi` crate, recorder service, commands/tray/quit guard).
- Task 6 (macOS tap + AX reader) is delegated to a T3 thread on the MacBook; on macOS the Teams source is a
  `Silence` placeholder until that work is merged into `meetings/commands.rs::make_sources`.
- Done since: Tasks 9 (Claude client), 10 (enhanced notes), 11 (UI + Settings), 13 (Windows live).
- Windows live (`WREN_LIVE=1 cargo test --test meeting_live -- --ignored --nocapture`): 3/3 clips via system audio,
  dictation p50 219 → 224 ms with a meeting running, WAVs aligned, Medium note via Opus 5.5 in 8.3 s (~$0.02).
- Installed on this PC: local build `rel-20261124-210444-b3c728e4` (v0.1.0 + Meetings). Don't click the sidebar
  Update pill until the Meetings release ships (it would install CI v0.1.3, which has no Meetings).
- Claude API key is saved in Windows Credential Manager (service "Wren", account "anthropic-api-key").
- Task 12 (macOS build + live) is running in a T3 thread on the MacBook.
- Waiting on Maximilian: Task 14 — a real Teams call with ≥ 1 other person on each machine, while
  `scripts/teams-call-probe.ps1` captures accessibility dumps (Windows; the Mac thread does the same there).
- Task 14 (Windows): one-person Teams call — 75 dumps; parser now reads the real self tile, mute button, title.
  Remote "speaking" label still unconfirmed (no second person).
- Task 15 council round 1: GPT Sol 4/10, Claude Opus 6/10, Gemini 4 Neon 6/10. All shared/Windows/UI fixes done
  (commits 5065cc7 … 14fb0d2); macOS-only fixes + a Mac Teams call running in a T3 thread on the MacBook.
  Maximilian's request: richer note formatting — done (headline box, action table, bold facts).
- Maximilian: the Mac app matters most (most meetings there); both apps must be bug-free.
- Council round 2 (on 627dc87): Opus 7/10, Sol 5/10, Gemini 7/10. Shared/Windows/UI fixes done (4d1434b, 9a0a848);
  macOS round-2 fixes running in a T3 thread on the MacBook.
- Council round 3 (on 0ec68be): Opus 8/10, Sol 5/10, Gemini 8/10. Shared/Windows/UI fixes done in d7751ea
  (mute fails closed before the first Teams read, the dictation record is kept for the whole meeting, quit waits
  for a starting meeting and cuts a long Stop short, the watcher is joined on stop, Windows reads every queued
  packet, notes flush on quit, auto notes wait 1.5 s). Verified: lib 396 passed, 0 warnings, 106 page checks,
  live Windows 3/3, p50 229→227 ms. macOS round-3 fixes running in a T3 thread on the MacBook (2026-11-25).
- Page check "day groups" was time-of-day flaky after midnight: it now accepts Today or Yesterday.
- Mac round-3 fixes merged (8af40b6), Windows re-verified. Council round 4 (at 8af40b6): Opus 8/10, Sol 5/10,
  Gemini 8/10. All three flag one shared privacy hole: a Me clip can be saved unmuted when the first Teams reads
  can't see the mute state. See the local ledger for the complete findings and rulings.
- **R4 shared, Windows and UI fixes complete (fb3e868, af85ec5, cc80e1f).** Me clips wait for a known mute read,
  saved Me rows in the 30 s before the first known read are reconciled and re-emitted, and unknown reads carry the
  last known mute state for 60 s. Stop drains the current Teams read before generating notes, and manual generation
  waits until the meeting ends. Confirmed quit stays latched through exit so an overdue start finishes as Ended
  without publishing live events. Terminal note events clear the generation snapshot first. Failed My notes drafts
  survive navigation and quit retries, tray Stop flushes immediately, and WASAPI drops its clients before leaving COM.
- **Windows verification:** Rust lib 402 passed, 15 ignored, 0 build warnings. npm build, JS unit and 114 page checks
  passed. Live 3/3 clips heard, dictation p50 220 ms idle → 228 ms recording. The live run used AllSound playback.
  The mute regressions used a fake Teams reader with SQLite, and the tray test simulated a throttled debounce.
  A real two-participant Teams call and native hidden-webview timing still need live confirmation.
- **R4 Mac fixes done by Sol (1dc8a7d, 248786c):** Mac lib 457 passed, 0 warnings, mac_tap live 8/8, live meeting
  3/3 with p50 1078→1085 ms (the Claude step was not run on the Mac: blocked by the approval check).
- **FINAL council round 5 (at 248786c): Opus 8/10, Sol 5/10, Gemini 8/10.** Still open: the 60 s mute carry-over
  only covers time inside a Teams reading (each ≤ 1 s), so a reading gap of over ~1 s while muted can save a line as
  unmuted (Sol critical, Gemini important; verified in `speakers.rs::muted_during`). Other open items are in the
  ledger ("Council R5 …"): ordinary Quit dropping a failed notes draft, Mac held-audio flush at a guessed rate,
  unbounded WASAPI activation wait, a 3 s wait for every Me line after a call ends, a double 15 s wait on quit.
- **STOPPED (2026-11-25):** waiting for Maximilian's decision. Nothing merged to main.

## Status (2026-11-24) — Meetings feature: spec APPROVED (history)

### Completed
- Reverse-engineered Granola (static only; couldn't log in) and Notion AI Meeting Notes (static, no recording started):
  `docs/research/granola-teardown.md`, `docs/research/notion-meeting-notes-teardown.md`.
- Key takeaways: both capture mic and system audio as two separate 16 kHz streams (mic = "Me", system = "Them");
  both detect calls by "a known call app is holding the mic"; both get real names from the call app's UI
  (Granola reads Teams' active speaker via UI Automation on Windows / Accessibility on macOS); both do
  enhancement server-side (prompt text not in their bundles). Granola keeps "My notes" and "Enhanced notes" as separate tabs.

### Decisions so far (Maximilian, 2026-11-24)
- Council at the end: **regular** (Claude Opus 5.5, GPT Sol, Gemini 4 Neon, effort high), reviewing for Windows + macOS bugs; Mac testing via a T3 thread on "Lord's MacBook Pro" (project `/Users/maximilian/Code/Wren`).
- Meetings to support: **Teams desktop app only**.
- Enhancement uses his Anthropic API credit (Claude API, default model `claude-opus-5-5`; Rust has no official SDK, so raw HTTPS via reqwest).

### Next step
- Spec: `docs/superpowers/specs/2026-11-24-meetings-design.md` (builder-checked). His answers: manual start only (no auto-detect), local Whisper, Teams active-speaker names, keep audio 24 h, Short/Medium/Long enhanced notes. Then: muted-in-Teams lines excluded from notes (1a); no transcription notice (2a). Spec approved. Then approval → writing-plans → build on a feature branch → council → release.

Wren is Maximilian's free, local dictation app for Windows and macOS Apple Silicon (a Willow / Glaido replacement).

## Status (2026-11-24) — macOS Apple Silicon Port Verified Live & Released

### Completed
- **Full macOS Apple Silicon (`aarch64-apple-darwin`) port** matching Windows `v0.1.3` with zero Windows regressions:
  - **Local MLX speech engine (`sidecar/wren_sidecar/mlx_backend.py`, `sidecar/macos/{pyproject.toml,uv.lock}`, `scripts/setup-runtime.sh`):** Private Python 3.12 runtime under `~/Library/Application Support/Wren/runtime/` running `whisper-large-v3-turbo-q4` on Apple Silicon GPU (`mlx`, ~452 MB unified memory) with `small.en` fallback, glossary prompt biasing, temperature fallback, and `BrokenPipeError` clean exit.
  - **Global `Fn + Control` hotkey (`src-tauri/src/hotkey/macos.rs`):** CoreGraphics `CGEventTap` (`kCGEventTapOptionDefault`, Accessibility-gated) feeding the shared `ChordMachine` for hold-to-talk, double-tap hands-free, and window-tiling cancellation. Ignores Apple Silicon Globe/Fn/Control `KeyDown`/`KeyUp` keycodes (`59`, `63`, `179`) and gates device-specific Control flag bits on `MASK_CONTROL`.
  - **Non-activating floating popup (`src-tauri/src/popup.rs`):** Runtime-verified `WrenPopupPanel : NSPanel` (`NonactivatingPanel`, `NSStatusWindowLevel`, `CanJoinAllSpaces | FullScreenAuxiliary`) with `LSUIElement = true` in `Info.plist` and active-window display placement so the pill never steals keyboard focus.
  - **Clipboard backup, delayed-render read receipts & `⌘V` paste (`src-tauri/src/paste/clipboard_macos.rs`, `src-tauri/src/paste/mod.rs`):** `NSPasteboardItemDataProvider` with `TransientType` / `ConcealedType` / `AutoGeneratedType` privacy markers, serialized clipboard transactions, `changeCount`-guarded restore, and main-thread-cached keyboard-layout `'v'` keycode resolution (`CACHED_V_KEYCODE`).
  - **Accessibility pre-paste check & correction watcher (`src-tauri/src/watcher/ax_macos.rs`):** `AXUIElement` focus inspection with `AXManualAccessibility` hydration for Electron/Chromium apps and `AXNumberOfCharacters` guard for huge documents.
  - **App icons, menu bar tray, themes, and macOS self-updater (`src-tauri/src/{app_icons.rs,tray.rs,lib.rs,updater.rs}`):** `NSWorkspace` `.icns` rasterisation on the main thread, menu bar tray + `RunEvent::Reopen`, macOS `NSWindow` light/dark appearance sync, and atomic `.app.tar.gz` self-update staging with rollback.
  - **Frontend parity & dual-OS CI (`.github/workflows/release.yml`, `scripts/ci-release-meta.mjs`, `scripts/release.sh`):** Platform-aware `Fn + Control` / `⌘F` / `Start at login` / `Apple Silicon (MLX)` UI copy, Accessibility onboarding banner, signed local `scripts/release.sh`, and unified GitHub Actions builds publishing both `windows-x86_64` and `darwin-aarch64` in `latest.json`.

### Live macOS fixes caught & verified with Maximilian
1. **Global tap outside Wren (`9cc7bba`):** Removed the `kCGEventTapOptionListenOnly` fallback in `hotkey/macos.rs` so a missing Accessibility grant surfaces clearly instead of creating a self-only tap.
2. **Paste-time Carbon HIToolbox crash (`08f5b08`):** Moved `TISCopyCurrentKeyboardLayoutInputSource` / `TISGetInputSourceProperty` / `LMGetKbdType` strictly onto the main thread (`paste::refresh_v_keycode()`), eliminating `EXC_BREAKPOINT` (`_dispatch_assert_queue_fail`) on `wren-finisher`.
3. **Double-tap hands-free latching (`f6eaa71`):** Ignored Apple Silicon Globe `KeyDown` (`keycode 179`, observed live in `wren.log`) plus `59`/`63` in `hotkey/macos.rs` so tapping `Fn + Control` twice reliably enters `HandsFree` without cancelling the first tap or clearing `AwaitDouble`.

### Verification
- `cargo test --manifest-path src-tauri/Cargo.toml --lib`: `259 passed; 0 failed; 1 ignored`.
- `WREN_LIVE=1 cargo test --manifest-path src-tauri/Cargo.toml --test engine_roundtrip`: `3 passed` (`large-v3-turbo-q4` on `mlx`, `452 MB`, p50 `1069 ms`, glossary biasing, crash recovery, orphan cleanup, zero dictated words in `sidecar.log`).
- `pytest sidecar/tests`: `38 passed`; `node scripts/unit-tests.mjs`: `20/20 PASS`; `node scripts/check-pages.mjs`: all passed; `--popup-check`: passed.
- Live verification on `/Applications/Wren.app` (`rel-20261124-142511-8d868e82`, signed with Apple Development cert): hold-to-talk, double-tap hands-free (3/3 after `keycode 179` filter), `⌘V` paste with `NSPasteboardItemDataProvider` read receipts (`25–57 ms`), and menu-bar background operation all verified live in T3 Code.

## Status (2026-11-23) — In-app updater silent no-op bug fixed & verified live (`v0.1.3`)

### Root cause
- Clicking **Update** in the sidebar downloaded and verified the signed NSIS installer, logged `update verified; exiting so the installer can finish`, and quit Wren — but `apply-update.ps1` never ran, `%LOCALAPPDATA%\Wren\logs\updater.log` was never created, and the installer + script stayed stranded in `%LOCALAPPDATA%\Wren\updates\`. Reopening Wren launched the unchanged `0.1.0` binary, which kept showing the Update button in an endless loop.
- **Why:** `spawn_install_helper` in `src-tauri/src/updater.rs` passed `CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` (`0x0800_0208`) to `powershell.exe`. Per Win32 `CreateProcessW` semantics, `CREATE_NO_WINDOW` is ignored whenever `DETACHED_PROCESS` is set; Windows PowerShell 5.1 launched consoleless under `DETACHED_PROCESS` with null stdio exits immediately with status `0` without executing `-File`.
- Reproduced directly on Windows 11: `0x08000208` → `rc=0, log_exists=False, script_exists=True`; `0x08000200` (`CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`) → `rc=0, log_exists=True, script_exists=False` (survives parent process exit 150 ms later).

### Completed
- **Fixed `spawn_install_helper` (`src-tauri/src/updater.rs`, commit `fe80a97`):** Removed `DETACHED_PROCESS` so `powershell.exe` runs with `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` (`0x0800_0200`).
- **Hardened `apply-update.ps1` & `UpdaterService::install` (`src-tauri/src/updater.rs`):**
  - Added early guard in `install()` against re-entry while `Downloading` or `Installing`.
  - Guarded `Wait-Process` with `if ($ParentPid -gt 0)` (so PID `0` never waits on Windows Idle) and added a post-timeout `Stop-Process -Force` fallback if the old `wren.exe` hangs past `UPDATE_WAIT_PARENT_SECS`.
  - Cached `$null = $proc.Handle` before `$proc.WaitForExit()` so `$proc.ExitCode` is never `$null` in Windows PowerShell 5.1, and launched the installer with `-WindowStyle Hidden`.
  - Added a post-install relaunch check: if the installer exits `0` but `wren` hasn't reappeared within 3 s via NSIS `/R`, `apply-update.ps1` starts `$FallbackExe` directly.
- **Added live integration test:** `updater::tests::spawn_install_helper_executes_installer_logs_and_cleans_up` spawns a real dummy parent + `spawn_install_helper` against a temp `.cmd` installer and asserts `updater.log` records exit code `0`, `/S /UPDATE /R` were passed, and both the installer and `apply-update.ps1` are cleaned up.
- **Released & verified live:** Pushed `fe80a97` to `main`, waited for CI to publish signed **`v0.1.3`**, installed the fixed local build via `scripts\release.ps1 -Launch`, and executed the full live update from `0.1.0` → signed `v0.1.3` using the exact `apply-update.ps1` + `0x08000200` spawn flags.

### Explicitly NOT changed
- No frontend, NSIS template, or floating popup changes were needed (`installer.nsi` `/S /UPDATE /R` and `RunAsUser` worked cleanly once `apply-update.ps1` actually ran).
- Local `scripts/release.ps1` still builds `0.1.0` from `tauri.conf.json`, so any future local `release.ps1` install will again offer the latest CI `0.1.<run>` version in-app until you update onto the CI build.

### Verification (raw output)
- `cargo test --offline --manifest-path src-tauri\Cargo.toml --lib`:
  `test result: ok. 259 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out; finished in 17.58s` (including `updater::tests::spawn_install_helper_executes_installer_logs_and_cleans_up ... ok`).
- GitHub Actions on `fe80a97`:
  - `Windows frontend build` (`45193333112`): `completed success` (`47s`).
  - `Build and release Wren` (`45193333192`): `completed success` (`7m41s`), published **`Wren v0.1.3`** (`2026-11-23T14:29:04Z`).
- `scripts\release.ps1 -Launch`: built `rel-20261123-152148-79b5c0e5`, `SELFTEST PASS` (`large-v3-turbo ready on cuda in 10405 ms (963 MB VRAM); warm-up clip transcribed in 355 ms`), `RELEASE OK`.
- Live end-to-end self-update test (`0.1.0` → signed `v0.1.3` via `0x08000200` + `apply-update.ps1`):
  - `%LOCALAPPDATA%\Wren\logs\updater.log`: `Starting silent installer: C:\Users\maxha\AppData\Local\Wren\updates\Wren_0.1.3_x64-setup.exe /S /UPDATE /R` → `Installer finished with exit code 0`.
  - `%LOCALAPPDATA%\Wren\updates\` remaining files: `0`.
  - Installed binary `FileVersion`: `0.1.3`; Registry `DisplayVersion`: `0.1.3`.
  - Relaunched automatically (`wren.exe` pid `37328`): `Wren starting` (`version="0.1.3"`, `build="wren-build-id:ci-0.1.3-fe80a97"`), `speech engine ready` (`large-v3-turbo`, `cuda`, `1585 MB VRAM`), followed 15 s later by `Wren is up to date` (`current="0.1.3"`).

### Next step
- Maximilian's PC is now running signed **`Wren v0.1.3`** (both the Light/Dark UI redesign and the working self-updater), up to date with no stuck Update pill in the sidebar. Future pushes to `main` will update cleanly in one click.

## Status (2026-11-23) — Complete Light & Dark UI/UX Redesign Shipped

### Completed
- Implemented Maximilian's approved UI/UX overhaul across all five main-window screens in both **Dark Mode** (slate-navy `#1c232d` ground, soft gradient sidebar `#222e3b → #202630`) and **Light Mode** (`#fbfcff` ground, misty `#e9f0f6 → #f0f2f7` sidebar) with semantic violet, teal and amber accents throughout (`src/styles/tokens.css`, `src/styles/base.css`, `src/styles/components.css`, `src/styles/pages.css`).
- **Dynamic Light / Dark / System theme + Windows 11 titlebar sync:**
  - Persisted `theme?: "dark" | "light" | null` in `settings.json` (`src-tauri/src/settings.rs`), with an anti-flash pre-paint script in `index.html`, `src/lib/theme.ts`, top-right quick `Dark | Light` toggle in `src/App.tsx`, and `Appearance` (`Dark | Light | System`) in `src/pages/Settings.tsx`.
  - Replaced hardcoded `light_caption` with dynamic `apply_window_theme` in `src-tauri/src/lib.rs` and `set_window_theme` command in `src-tauri/src/commands.rs` (sets `DWMWA_USE_IMMERSIVE_DARK_MODE`, `DWMWA_CAPTION_COLOR`, `DWMWA_BORDER_COLOR`, `DWMWA_TEXT_COLOR` + Tauri window theme on startup and immediately when toggled).
- **Real Windows app icons + inline WAV playback:**
  - Added `src-tauri/src/app_icons.rs` (`app_icon` command) using Win32 `SHGetFileInfoW` + `GetIconInfo` + `GetDIBits` and a pure-Rust zero-dependency PNG encoder (verified with `tauri::image::Image::from_bytes`) to extract and cache authentic 32×32 icons for any Windows executable, paired with bundled crisp PNG/SVG brand artwork in `src/lib/appIcons.ts` and `src/lib/appIconPngs.json`.
  - Added `history_audio_ids` and `history_audio` commands (`src-tauri/src/store/mod.rs`, `src-tauri/src/commands.rs`) so History transcript cards display an inline play/pause seek bar whenever a local WAV recording exists on disk.
- **Redesigned screens:**
  - **Home (`src/pages/Home.tsx`, `src/components/WaveHero.tsx`):** Calm `Hi, Maximilian.` greeting, full-width waveform `Dictation activity` hero showing live **Time saved** (last 7 days, reactive to Settings typing speed), **Speaking speed**, and **Current streak**, followed by **Recent dictations** in softly tinted hairline cards with real app icons and a `View history →` link. Removed legacy unused `StatTile.tsx`, `DayStrip.tsx`, and `Card.tsx`.
  - **History (`src/pages/History.tsx`, `src/components/TranscriptCard.tsx`):** Search bar (`Ctrl+F` / `/`, `Escape` clears, violet search icon) + day filter dropdown (`All days`, `Today`, `Yesterday`, …), teal retention label, day-grouped hairline cards with real app icons, search `<mark>` highlights in both app names and text, Rust-safe click-to-copy, Fix a word, Delete confirm, and inline audio playback.
  - **Dictionary (`src/pages/Dictionary.tsx`):** Per-tab semantic accent colours (`Words` teal, `Replacements` violet, `Learned` amber), icon tabs with keyboard arrow navigation, tinted add forms, and tinted gradient list cards with `.tinted-icon` wells while preserving duplicate validation, toggle switches, candidate Keep/Undo, and Delete confirm.
  - **Stats (`src/pages/Stats.tsx`, `src/components/BarChart.tsx`, `src/components/LineChart.tsx`):** 3 pastel-tinted semantic summary tiles (`Words dictated` teal, `Time saved` violet, `Current streak` amber), violet `Words per day` chart with `7 days | 30 days` range toggle and dashed average line, amber `Time of day` bar chart, and teal `Speaking speed` trend chart with typing baseline — sized to fit cleanly above the fold at `1200×800` with `0 px` horizontal overflow.
  - **Settings (`src/pages/Settings.tsx`):** Quick section jump bar (`Appearance · Microphone · Dictation · Learning · AI clean-up · Speech engine · History · Updates`), new `Appearance` group, semantic tinted icon wells and gradient cards, teal 28-segment live VU meter, and all existing real settings preserved (mic test, typing speed validation, correction watcher, Ollama AI clean-up, engine specs & restart, delete history confirm, other dictation apps, GitHub updater).
  - **Sidebar (`src/components/Sidebar.tsx`):** Soft gradient rail, semantic active page icon accents, theme-adaptive `Update · vX.Y.Z` pill, and preserved `Ready` / `Speech stopped` engine footer.

### Explicitly NOT changed
- **Floating recording popup pill (`src/popup/`, `popup.html`):** Left 100% untouched per Maximilian's instruction (zero diff).
- **Deferred glitches 1 & 2** listed further down remain intentionally deferred.
- Temporary prototype directory `docs/design/proposals/` was removed before committing so `main` stays lean.

### Decisions & lessons learned
- Added `"Win32_Storage_FileSystem"` to `windows` crate features in `src-tauri/Cargo.toml` (required by `SHGetFileInfoW`, already in local offline registry cache).
- Fixed a subtle CSS overflow bug where `table.sr-only` inside charts ignored `width: 1px` due to `display: table` and inflated `.panel.scrollWidth` by 58–178 px; setting `display: block; clip-path: inset(50%); top: 0; left: 0;` on `.sr-only` and `position: relative;` on `.chart` brought horizontal overflow to `0 px` on every page and viewport.
- Wired Settings → Appearance directly to `AppShell.switchTheme` so changing Light/Dark/System updates the UI and Windows caption bar in 0 ms while persisting `settings.json` in the background.

### Verification (raw output)
- `cargo test --offline --manifest-path src-tauri\Cargo.toml --lib`: `258 passed; 0 failed; 11 ignored; 0 measured; 0 filtered out` (includes 3 new `app_icons` tests + `settings::theme` round-trip/default tests).
- `npm run build` (`tsc && vite build`): `✓ built in 819ms`.
- `node scripts/unit-tests.mjs`: `20/20 PASS` (`all unit tests passed`, including `theme`, `computeTimeSavedMs`, `appName`, and Rust-only clipboard guard).
- `node scripts/nsis-template.mjs --check`: `NSIS template up to date`.
- `node scripts/check-pages.mjs`: `57/57 PASS` (`all checks passed`, including 10 new checks covering waveform hero, real app icons, Dark/Light/Appearance theme switching, Home → History link, History day filter, and Stats 7d/30d toggle).
- Playwright visual + overflow sweep (`1200×800` & `960×640`, Dark & Light): `16/16 checks`, `overflowIssues=0`, visually inspected every screen in both themes.
- `powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Launch`: built `Wren_0.1.0_x64-setup.exe` (`4.1 MB`, `rel-20261123-145812-745e1949`), installed to `%LOCALAPPDATA%\Programs\Wren\wren.exe`, `SELFTEST PASS` (`large-v3-turbo ready on cuda in 12790 ms (1235 MB VRAM); warm-up clip transcribed in 453 ms`), relaunched in tray (`pid 24356`), `RELEASE OK`.

### Next step
- Maximilian can click the Wren tray icon right now to open the newly installed build and try Dark/Light mode, real Windows app icons, History audio playback, and the redesigned screens live.
- Push to `origin/main` triggers `.github/workflows/release.yml` to publish the signed GitHub Release for this redesign.

## Status (2026-11-02, overnight build in progress)

- Brainstorm finished in chat with Maximilian (egm-brainstorm). Concept: `docs/design/concept-input.md`.
- He approved an unattended overnight build: "I approve anything to make this app; just tell me everything
  that happened afterwards."
- Running now: Goldfish concept review, Willow teardown, Glaido teardown, speech-model benchmark.

## Decisions made with Maximilian (brainstorm, 2026-11-02)

- Hotkey: Left Ctrl + Left Win only. Hold to talk; double-tap for hands-free. No other global hotkeys.
- Speech-to-text runs locally on his RTX 3060 Ti. Completely free. English only (British accent).
- Standalone app, unrelated to Overseer.
- Always-on instant tidy-up (fillers, stray "i"). AI clean-up is a switch, off by default.
- No agent commands, no custom tools, no snippets, no per-app styles, no "longest dictation" stat.
- Dictionary that learns from corrected words; must work in the Claude desktop app if possible.
- History searchable, text + audio deleted after 3 days; daily totals kept forever for stats.
- Stats: time saved, WPM, streak, busiest hour, daily-use chart.
- Home: big stats on top, recent transcriptions as cards; click a card = copy to clipboard.
- Look: Glaido's layout (big, uncluttered) + Willow's light theme and finish. Popup sized like Glaido's.
- Name: Wren (picked by Claude; he can veto).
- OK to use his ~1,100 Willow recordings to benchmark models.

## Decisions made by Claude (and why)

- Tauri 2 (Rust + WebView2) + Python sidecar for speech. Rust gives a reliable low-level keyboard hook and
  small footprint (Glaido is Tauri); Python faster-whisper on CUDA is already proven on this PC (Overseer).

## Status (2026-11-02 ~10:10 UTC) — BUILT, INSTALLED, RUNNING

- All 11 plan tasks done, each reviewed (spec + quality) with fix rounds; UX polish pass; final whole-branch
  review (ready with fixes) → one fix wave → scoped re-review. Main = merge of `final-fixes`.
- Installed at `%LOCALAPPDATA%\Programs\Wren\wren.exe` (NOT `%LOCALAPPDATA%\Wren`, which is the data root —
  Tauri's per-user default would have put it there; uninstall would have deleted the data). Running in the tray.
- Verified on the installed build: self-test PASS; live test PASS (holds 10/10, hands-free, composer, focus never
  stolen, Start 0/50, release→paste p50 ~324 ms, Win+V history untouched, graceful quit, no orphan python);
  Task 9 live (learning 0.17 s, not-editable → copy); popup click rounds (copy / Undo / ×).
- Tests: cargo lib 248 pass; sidecar pytest 39; page checks 42; unit 11.
- Morning note for Maximilian: `docs/MORNING.md`. Every ruling: `.superpowers/sdd/PLAN/progress.md` (git-ignored
  ledger) — the morning note lists the ones that matter to him.

## Left / follow-ups

Maximilian, 2026-11-02 morning: using Wren, "working perfectly well"; **don't fix the two engine-failure glitches
for now — just keep them noted.** Start with Windows is on in Wren; he took Willow/Glaido out of startup.

- **Deferred glitch 1 — GPU out of memory (final review M-4):** an `Oom` from the sidecar is retried once on the
  same, still-Ready engine (`session.rs` ~1008-1029), then saved without text. With Unity or a big Ollama model
  filling VRAM, every dictation fails until Settings → Restart engine. Proposed fix: restart the engine (it falls
  back to CPU small.en if the card is still full) before the retry, and say "graphics card is full" in the error.
- **Deferred glitch 2 — lingering invisible popup (fix-wave re-review):** when the engine is Failed, the
  engine-stopped error shown during a hold has no hide time; if the recording then ends via the discard path (tap,
  Ctrl+Win+→, Cancel, Pause, quiet save — `session.rs` ~504-509) the window stays shown and clickable after the
  page fades it: an invisible pill-sized area at the bottom centre swallows clicks (opens Wren) until the next popup
  state. Proposed fix: in `recording_ended()`, also hide when the current state is an error with no hide time.
- Fix-a-word Undo leaves that dictation's corrected text in History (needs a `history_set_text` command).
- Not verifiable by automation (he checks): real-keyboard Start menu, Win+Shift+S / Win+V passthrough, first-word
  clipping with the real mic, accent accuracy live in Claude.
- Rebuild/reinstall: `powershell -ExecutionPolicy Bypass -File scripts
elease.ps1` (runs every step via WMI,
  outside the Claude app's MSIX sandbox). Don't run `cargo fmt` on the crate (reformats ~40 files).

## Status (2026-11-23) — In-app GitHub updater added

### Completed
- Added a full in-app updater backed by GitHub Releases (`src-tauri/src/updater.rs`, `src-tauri/src/commands.rs`,
  `src-tauri/src/tray.rs`, `src-tauri/src/constants.rs`, `src-tauri/src/paths.rs`, `src-tauri/src/lib.rs`).
- **Private repo today, public repo later with zero changes:** checks the unauthenticated public
  `releases/latest/download/latest.json` endpoint first (zero API rate limit once open-sourced); on 404 it
  automatically falls back to the GitHub REST API using `WREN_GITHUB_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`
  (run silently with `CREATE_NO_WINDOW`) and strips the `Authorization` header on cross-host blob redirects.
- **Cryptographic verification + clean silent install:** every downloaded NSIS installer is verified with
  Ed25519 minisign (`minisign-verify`, matching `tauri signer sign --app-version <ver>` version binding) against
  `UPDATE_PUBLIC_KEY` before anything executes. Installing writes a small detached PowerShell helper under
  `%LOCALAPPDATA%\Wren\updates\apply-update.ps1` that waits for `wren.exe` to shut down cleanly (unhooking the
  keyboard and stopping the Python sidecar), runs `/S /UPDATE /R` (preserving `%LOCALAPPDATA%\Programs\Wren` and
  all user data), logs to `%LOCALAPPDATA%\Wren\logs\updater.log`, and relaunches Wren automatically.
- **Non-intrusive UI (similar to T3 Code):**
  - Sidebar (`src/components/Sidebar.tsx`, `src/styles/components.css`): completely hidden when up to date;
    shows a compact soft-indigo `Update · vX.Y.Z` button when a newer GitHub release is available. One click
    downloads with in-place progress fill (`Updating · 42%`), verifies, and restarts into the new version.
  - Settings (`src/pages/Settings.tsx`, `src/styles/pages.css`): dedicated `Updates` section with current
    version, manual `Check for updates`, release notes, progress bar, and error/retry controls. Also added
    `scroll-margin-top: 24px` to `.setgroup__title` so focus-scrolling into Settings groups never clips.
  - System tray (`src-tauri/src/tray.rs`): `Check for updates` item updates live to `Update available (vX.Y.Z)` /
    progress and jumps straight to Settings → Updates.
- **GitHub Actions CI release pipeline (`.github/workflows/release.yml`, `scripts/ci-release-meta.mjs`):**
  - Triggers on pushes to `main` (ignoring `docs/**` and `README.md`) and `workflow_dispatch`.
  - Auto-stamps `0.1.<run_number>` (or explicit manual version), builds the NSIS installer with
    `CARGO_TARGET_DIR` overriding `.cargo/config.toml`, signs with `npx tauri signer sign --app-version`, writes
    `latest.json`, and publishes the GitHub Release via `gh release create`.
- Generated the Ed25519 minisign keypair, uploaded `TAURI_SIGNING_PRIVATE_KEY` to GitHub Actions secrets on
  `Kamo2323/wren`, and backed up the keypair outside the repo at `%USERPROFILE%\.wren\updater.key` (`.pub`).
- Fast-forwarded local `main` to `origin/main` (`b4787a6`) before committing.

### Verification (raw counts)
- `npm run build`: succeeded cleanly (2.35s).
- `node scripts/unit-tests.mjs`: 18/18 passed (`all unit tests passed`).
- `node scripts/nsis-template.mjs --check`: `NSIS template up to date`.
- `node scripts/check-pages.mjs`: 47/47 passed (`all checks passed`, including 5 new updater UI checks).
- `cargo test --offline --manifest-path src-tauri\Cargo.toml --lib`: `255 passed; 0 failed; 11 ignored`
  (including 7 new updater tests covering semver, minisign + version binding, HTTPS validation, apply script,
  public download, private API fallback with auth-stripped redirect, and no-releases-yet handling).
- `scripts\release.ps1`: built `Wren_0.1.0_x64-setup.exe` (4.1 MB, `rel-20261123-035944-795d0c9a`), installed to
  `%LOCALAPPDATA%\Programs\Wren\wren.exe`, `SELFTEST PASS` (CUDA `large-v3-turbo`, 452 ms), `RELEASE OK`,
  restarted Wren in tray.

### Status / next steps
- Pushed to `origin/main` to trigger the first `.github/workflows/release.yml` Windows build & GitHub Release.
- Once that GitHub Actions run publishes `v0.1.<run_number>`, the installed Wren (v0.1.0) will detect it
  automatically (or immediately via Settings → Updates → Check for updates) so Maximilian can test the
  one-click self-update end to end.

### Decisions & lessons learned
- Built a custom Rust updater module (`reqwest` + `minisign-verify` + `semver`) instead of `tauri-plugin-updater`
  because `tauri-plugin-updater` cannot download assets from private GitHub repos (where `github.com/.../download`
  returns 404 and the GitHub REST API asset endpoint requires `Accept: application/octet-stream` + stripping
  `Authorization` on redirect). Using the exact Tauri `latest.json` + minisign format gives full compatibility
  plus seamless private→public transition.
- `.cargo/config.toml` is tracked and pins `target-dir = "C:/Wren/.cargo-target"`, so `.github/workflows/release.yml`
  sets `CARGO_TARGET_DIR: ${{ github.workspace }}\src-tauri\target` to keep builds inside the runner workspace and
  aligned with `Swatinem/rust-cache`.
- Do NOT put `createUpdaterArtifacts: true` permanently in `tauri.conf.json`, or local `scripts/release.ps1`
  would fail without the private signing key; CI runs `npx tauri signer sign --app-version` explicitly instead.

## Things to tell Maximilian (all in docs/MORNING.md)

- Glaido closed (not uninstalled); Willow wasn't running; both should come out of Windows startup before he turns
  on Wren's "Start with Windows".
- Win+V history lost ~12 older unpinned entries during Task 5's first tests (root-fixed).
- Notepad reopened his FILM5-DESIGN.md tab (unchanged); someone pressed real keys at 03:07 Prague; two OpenWith
  pickers closed; the Glaido study sent 5 silent clips to Glaido's servers.
- Downloads: models 2.0 GB, runtime 2.3 GB, Ollama qwen2.5:3b 1.9 GB, Handy source (MIT) for reference.
