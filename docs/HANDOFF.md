# Wren — handoff and decision log

Wren is Maximilian's free, local Windows dictation app (a Willow / Glaido replacement). Repo: `C:\Wren`.

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
