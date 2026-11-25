# Wren

Wren is a free, local dictation app for Windows. Hold a key chord, speak, let go, and what you said
is typed into whatever app you were using. The speech recognition runs on your own graphics card
(faster-whisper `large-v3-turbo` on CUDA): nothing you say leaves the PC, and there is nothing to pay.

It is built with Tauri 2 (Rust + WebView2, React front end) and a small Python "sidecar" process
that runs the speech model. The design and every rule it follows are in
[`docs/design/SPEC.md`](docs/design/SPEC.md).

## Using it

| Do this | What happens |
| --- | --- |
| **Hold Left Ctrl + Left Win**, speak, let go | Wren records while you hold, then pastes the text where your cursor is |
| **Double-tap** Left Ctrl + Left Win | Hands-free: Wren keeps recording with nothing held down |
| **Tap once** (while hands-free) | Stops and pastes |
| Tap and let go quickly (under 0.4 s) | Nothing happens — taps never record or paste |

- A small popup at the bottom of the screen shows that Wren is listening, then transcribing. It never
  takes the focus away from the app you are typing in.
- If nothing editable has the focus when the text is ready, Wren copies it instead of pasting and the
  popup says so. Your own clipboard is put back afterwards, and Wren's text is kept out of the Win+V
  clipboard history.
- Other shortcuts that use those keys (Ctrl+Win+Left/Right, Win+Shift+S, Win+V) keep working, and the
  Start menu does not open when you let go of the chord.
- Wren lives in the system tray. Closing the main window hides it; **Quit** is in the tray icon's menu.
  Click the tray icon to open the main window:
  - **Home** — today's numbers and your recent dictations (click a card to copy it).
  - **History** — searchable list of everything from the last 3 days, with the audio.
  - **Dictionary** — words Wren should spell your way, and words it has learned from your
    corrections (fix a word straight after a paste and Wren learns it; a "Learned" toast lets you
    undo that).
  - **Stats** — time saved, words per minute, streak, busiest hour, daily use.
  - **Settings** — microphone; dictation (your typing speed for "time saved", sounds, start with
    Windows); learning from corrections; the optional AI clean-up; the speech engine; history.

## Installing and updating

### Updating from inside Wren

Wren checks GitHub Releases silently on start-up (after a short delay) and every hour:

- When a new release is available, a compact **Update · vX.Y.Z** button appears quietly at the bottom of
  the sidebar (and in **Settings → Updates** and the tray menu).
- Click it once to download the signed installer, verify its Ed25519 signature, install silently over
  `%LOCALAPPDATA%\Programs\Wren\`, and relaunch Wren. Your history, dictionary, settings and speech
  models stay untouched.
- Pushing changes to `main` on GitHub triggers [`.github/workflows/release.yml`](.github/workflows/release.yml),
  which builds the Windows NSIS installer and the Apple Silicon app, signs the update files with the
  `TAURI_SIGNING_PRIVATE_KEY` Actions secret, and publishes the GitHub Release (`latest.json` +
  installers + `.sig`). Authenticode and Developer ID signing/notarization are opt-in and fail closed:
  see [`docs/RELEASING.md`](docs/RELEASING.md).
- Update checks are anonymous: Wren reads the public `latest.json` and never uses a GitHub token or the
  `gh` CLI. Only in **private mode** (`WREN_UPDATE_PRIVATE=1`, in the environment Wren runs in or at
  build time — for the owner's own machines while the repository is private) does it fall back to the
  GitHub API with `WREN_GITHUB_TOKEN` / `GITHUB_TOKEN` / `gh auth token`.
- Forks and unofficial builds choose their update source at build time: `WREN_UPDATE_REPO=owner/repo`
  (with `WREN_UPDATE_PUBKEY` for their own signing key), or `WREN_UPDATE_REPO=none` to build without
  updates. `WREN_DISABLE_UPDATER=1` stops the background checks of any build.

### Building and installing locally

From a terminal in the repo:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\release.ps1            # build, install, self-test
powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -Launch    # ...and start Wren in the tray
powershell -ExecutionPolicy Bypass -File scripts\release.ps1 -NoBuild   # reinstall the last build
```

`release.ps1` builds the front end and the release app, makes the NSIS installer, asks a running
Wren to quit, installs it silently for your user only (no admin prompt) and runs the installed
self-test. Running it again upgrades in place; your history, dictionary and settings stay. It prints
where everything went and ends with `RELEASE OK` or `RELEASE FAIL`.

The installer itself is `Wren_0.1.0_x64-setup.exe` (copied to `.release-out\` in the checkout, or
`-StageDir` / `WREN_STAGE_DIR`); double-clicking it does the same install with a normal setup window.
It puts a **Wren** shortcut in the Start menu. Builds go to `.cargo-target\` in the checkout unless
`CARGO_TARGET_DIR` is set. Nothing in a local build needs the release signing keys.

### First run: speech setup inside the app

The installer is small; the speech engine is not in it. On first launch Wren opens **Set up speech
recognition**, which lists what it will download and how big it is (on Windows about 3.6 GB: a private
Python, the engine's packages and two models; about 6 GB free needed while installing), checks free
space, and installs everything into `%LOCALAPPDATA%\Wren` (macOS: `~/Library/Application Support/Wren`)
with no admin rights, terminal, Python or uv needed. Every download is pinned and checksum-verified
([`sidecar/runtime-manifest.json`](sidecar/runtime-manifest.json)). It can be paused, and after a pause,
a dropped connection or a crash it carries on where it stopped. Setup only says ready once the real
speech engine has loaded and transcribed its test clip; it then offers a microphone check and a box to
try a dictation in. **Settings → Speech engine → Repair speech setup** re-checks every file and fixes
what is broken without touching a working install. Each run is logged to
`%LOCALAPPDATA%\Wren\logs\runtime-setup.log`.

Developers can still build the same runtime from a terminal (needs uv):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check-prereqs.ps1
powershell -ExecutionPolicy Bypass -File scripts\setup-runtime.ps1
```

**Start with Windows** is off by default. Turn it on in Settings once Willow and Glaido are no longer
starting with Windows (all three react to the same chord).

## Where things live

| What | Where |
| --- | --- |
| The program | `%LOCALAPPDATA%\Programs\Wren\` (`wren.exe`, the speech engine's Python code under `_up_\sidecar\`) |
| Settings | `%APPDATA%\Wren\settings.json` |
| History database (text, dictionary, daily totals) | `%LOCALAPPDATA%\Wren\db\wren.db` |
| Recordings (deleted with their text after 3 days) | `%LOCALAPPDATA%\Wren\audio\` |
| Speech engine (Python venv) and models | `%LOCALAPPDATA%\Wren\runtime\`, `%LOCALAPPDATA%\Wren\models\` |
| Unfinished speech-setup downloads (removed once setup succeeds) | `%LOCALAPPDATA%\Wren\setup-cache\` |
| Logs (timings only, never your words) | `%LOCALAPPDATA%\Wren\logs\` |
| Last self-test result | `%LOCALAPPDATA%\Wren\logs\selftest.txt` |

The program and the data are in different folders on purpose: reinstalling or uninstalling the
program can never touch the data.

## Checking it works

```powershell
& "$env:LOCALAPPDATA\Programs\Wren\wren.exe" --selftest | Out-Host   # prints each check, exit code 0 = pass
& "$env:LOCALAPPDATA\Programs\Wren\wren.exe" --quit                   # asks a running Wren to quit
```

The self-test checks the keyboard hook installs, a microphone records, the database is writable, and
the speech engine starts on CUDA and transcribes a bundled clip correctly. It ends with
`SELFTEST PASS` or `SELFTEST FAIL <reasons>` and also writes that to `logs\selftest.txt`.

The live end-to-end test drives the real app with injected key chords and a test recording instead
of the microphone, pasting into a test page (`scripts/test-page.html`) in an Edge window — never into
your own apps. It refuses to run if the PC is locked, if Willow or Glaido is running, or if Wren is
already running (quit it from the tray first):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1                       # builds a debug Wren and tests it
powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1 -Wren "$env:LOCALAPPDATA\Programs\Wren\wren.exe"
powershell -ExecutionPolicy Bypass -File scripts\live-test.ps1 -Wren "$env:LOCALAPPDATA\Programs\Wren\wren.exe" -Task9
```

It writes `docs/live-test-report.md`. The test uses its own temporary data folder, so it never adds
anything to your history.

Screenshots of the real installed app (from test recordings, not real dictations) are in
`docs/screenshots/real/`; `scripts/real-screens.ps1 -TestHome <live-test work folder>\home` takes
them again after a live test.

Developer checks: `cd src-tauri; cargo test` (Rust; `WREN_ALLOW_NO_RUNTIME=1` skips the few tests that
need an installed speech runtime), `node scripts/unit-tests.mjs`, `npm run build; node
scripts/check-pages.mjs` (UI checks in a browser), `cd sidecar; uv run pytest` (speech sidecar),
`npm run dev` / `npx tauri dev` (run from source).

## Uninstalling

Settings > Apps > Installed apps > **Wren** > Uninstall (or run
`%LOCALAPPDATA%\Programs\Wren\uninstall.exe`). That removes the program, the Start menu shortcut and
the start-with-Windows entry. The "delete the application data" tick box only removes WebView2's
cache (`%LOCALAPPDATA%\com.maximilian.wren`). Your history, dictionary, settings, the speech engine
and the models stay in `%APPDATA%\Wren` and `%LOCALAPPDATA%\Wren`; delete those two folders by hand
if you want them gone too.

## Known limits

- English only, tuned for a British accent.
- Needs an NVIDIA graphics card for full speed. Without CUDA the engine falls back to the CPU, which
  is much slower (the self-test reports this as a failure).
- The chord is fixed to Left Ctrl + Left Win.
- Willow and Glaido use the same chord: if either is running, both apps react. Close them (and take
  them out of start-up) before relying on Wren.
- History and recordings are kept for 3 days; only the daily totals behind Stats are kept for good.
- Learning from corrections needs the app to expose its text to Windows UI Automation. It works in
  normal text boxes, browsers and the Claude desktop app's composer; some apps do not expose their
  text, and then nothing is learned.
- The AI clean-up switch (off by default) needs Ollama running locally with a small model; it only
  tidies punctuation and wording, and it adds a little time to each dictation.
- Unless a release is built with Authenticode signing turned on (`docs/RELEASING.md`), the installer
  is not code-signed, so Windows SmartScreen may warn the first time you run it.

## License

MIT. See [LICENSE](LICENSE).
