# Wren — handoff and decision log

Wren is Maximilian's free, local Windows dictation app (a Willow / Glaido replacement). Repo: `C:\Wren`.

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

### Not done / next steps
- Not yet pushed to `origin/main` and not yet rebuilt/installed locally via `scripts/release.ps1` (waiting for
  Maximilian to review or ask to push + reinstall so his running tray copy picks up the updater).
- Once pushed to `main`, `.github/workflows/release.yml` will build and publish `v0.1.<run_number>` on GitHub;
  after reinstalling locally once (so the installed `wren.exe` has the updater code), future pushes to `main`
  will show the sidebar update button automatically.

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
