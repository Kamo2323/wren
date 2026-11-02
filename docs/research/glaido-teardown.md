# Glaido teardown (for Wren)

Target: Glaido 0.4.6 for Windows, `C:\Users\maxha\AppData\Local\Glaido\glaido-core.exe`
(SHA-256 `671ef0e2…86d47af`, 34.4 MB). Investigated 2026-11-02. Maximilian authorised it.

What this document is: a description of **how** Glaido solves each problem, with measurements
a designer can build from. None of Glaido's code or artwork goes into the Wren repo. The
screenshots in `docs/research/glaido/` are reference captures only. Do not ship them.

Evidence labels used below:

- **observed**: seen at runtime (window APIs, CDP, keyboard-hook log, Glaido's own log) or read
  directly from the shipped front-end bundle or the binary's strings.
- **inferred**: a reasonable conclusion from observed facts, not seen directly.
- **unknown**: not established.

Raw material lives in `C:\Wren\.research-cache\glaido\` (git-ignored). It holds the extracted
front-end assets (`assets/`), prettified bundles (`pretty/`), `strings.txt`, `crates.txt`,
`own-src.txt`, `pe-imports.json`, the run logs (`run_*`) and the scripts (`scripts/`).

---

## 1. Stack summary

| Layer | What Glaido uses | Evidence |
|---|---|---|
| Shell | Tauri **2.11.5**, wry 0.55.1, tao 0.35.3, webview2-com 0.38.2, tray-icon 0.24.1, muda (menus) | observed (crate source paths in binary) |
| Front end | React + Tailwind (shadcn/Radix-style `data-slot` components), Vite build, 14 UI languages. Four HTML entry points: `index.html` (main window), `overlay.html` (recording popup), `toast-overlay.html` (notifications), `command-modal.html` | observed (assets extracted from the exe; Tauri stores them brotli-compressed) |
| Tauri plugins | autostart, deep-link, opener, store, updater (minisign, `releases.glaido.com`), window-state | observed |
| Audio capture | `cpal` 0.15/0.16 over **WASAPI**; device native format (here 48 kHz, 2 ch, f32); channel mix; `rubato` resampler; `nnnoiseless` (**RNNoise**) denoiser on every capture; `rustfft`/`realfft` | observed (crates, log `Denoiser initialized: RNNoise`) |
| Audio encode / transport | **Opus** (`opus` crate) streamed over a **WebSocket** (`tokio-tungstenite`) to `wss://api.glaido.com`, with resume and replay. HTTP fallback `POST /transcription` and `/transcription/retry`. `flacenc` and `hound` (WAV) also present | observed (crates, strings `opus_resume.rs`, `ws_client/replay.rs`, `/transcription/retry`) |
| Speech-to-text | **Cloud.** The server names providers **Groq** and **Baseten** (Whisper-style `whisper_params`, `streaming_params`, `enable_vad`, `language_options`, `enable_retranscription`) | observed (strings); the model identity is inferred |
| Local storage | SQLite through `rusqlite`, **SQLCipher-encrypted** `glaido.db` (WAL mode). Key held in Windows Credential Manager (`keyring`, `CredReadW`). Audio kept as encrypted `*.wav.enc` | observed (file headers aren't `SQLite format 3`; `storage_mode=keychain` in log; cipher pragmas in strings) |
| Settings | Synced with the backend (`content_sync/settings_sync`). A small `settings.json` in Roaming holds only identity and onboarding flags. Hotkeys are stored in **macOS key-code and CGEventFlags vocabulary on every OS** | observed (`get_settings` output) |
| Win32 APIs | `SetWindowsHookExW` (WH_KEYBOARD_LL + WH_MOUSE_LL), `SendInput`, `GetAsyncKeyState`, clipboard APIs (`GetClipboardSequenceNumber`, `AddClipboardFormatListener`), `SetWindowDisplayAffinity`, DWM (`DwmSetWindowAttribute`, `DwmEnableBlurBehindWindow`), `WTSRegisterSessionNotification` (screen lock), COM (`CoCreateInstance`, used for WASAPI mute) | observed (PE import table) |
| Extras (not needed for Wren) | "Commands" agent mode with MCP servers, Claude Code/Codex hand-off, clipboard history, YouTube/web tools, referrals, billing | observed |

Rust module map (from embedded source paths): `hotkey/engine.rs`, `hotkey/engine/shared_key.rs`,
`platform/windows/{hotkeys,paste,clipboard_owner,selection,volume,window,screen_lock}.rs`,
`audio/{capture,denoise,channel_mix,pipeline,warmup,device_manager}.rs`,
`commands/recording/{start,stop,duration_limit,processing_limit,hotkey_router}.rs`,
`network/ws_client/{opus_audio,opus_resume,replay,upload}.rs`,
`dictionary/{terms,suggestions}.rs`, `history_archive/recording.rs`.

---

## 2. Behaviour questions

### 4a. Hotkey

**Mechanism (observed).** A dedicated thread installs a global **WH_KEYBOARD_LL** hook, plus
WH_MOUSE_LL for mouse-button hotkeys, and runs its own `GetMessageW` pump. Log strings:
"WH_KEYBOARD_LL hook installed", "exiting hotkey pump".

**Owner's bindings (observed via `get_settings`).** Keycodes are macOS virtual keys
(59 = Control, 56 = Shift). Flags are CGEventFlags (0x100000 = Cmd/Win, 0x80000 = Alt,
0x20000 = Shift, 0x40000 = Ctrl).

| Action | Binding | Activation |
|---|---|---|
| Dictation (hold) | Left Ctrl + Win | `press` (hold to talk) |
| Hands-free dictation | Ctrl + Win + Shift | `press` (toggle) |
| Commands (hold) | Left Ctrl + Left Alt | `press` |
| Hands-free commands | Left Ctrl + Left Alt + Shift | `press` |
| Enter to stop and paste | off | — |

**Activation modes (observed).** `press`, `double_tap_hold` ("Double tap and hold") and
`double_tap_toggle` ("Double tap to start"). The UI enforces that **double tap works only with a
single key, not a chord**. The engine's state machine has these states:

- `SourceDoubleTapPressed {pressed_at_ms}`
- `DoubleTapPrimed {released_at_ms}`
- `Holding {since_ms}`
- `HoldingGrace`
- `Persistent`

There is also a "**shared-key grace**" timer for bindings that share keys (Ctrl+Win and
Ctrl+Win+Shift). **Observed:** pressing Ctrl→Win→Shift showed the popup the moment Ctrl+Win
completed (36 ms). Shift then **promoted** the running session to hands-free, adding the close
button. Recording never restarts.

**Timing values for double-tap and grace windows: unknown.** They are Rust constants with no
string. Double-tap is not configured on this machine, and testing it would mean changing a
setting, which was out of bounds.

**Start-menu suppression (observed with my own LL hook and `GetAsyncKeyState`):**

- *Ctrl first, then Win* (normal order): Glaido **swallows the Win key-down and key-up** (it
  returns non-zero from the hook). The OS never sees Win as pressed, so Start can't open. Ctrl
  passes through normally. Shift, when it completes the hands-free chord, is also swallowed. No
  mask key is injected.
- *Win first, then Ctrl*: Win has already reached the OS. At the instant Ctrl completes the chord,
  Glaido **swallows Ctrl** and **injects a mask key: VK `0xFF` (unassigned), scan 0, down+up**,
  tagged with `dwExtraInfo = 0x474C4149` (ASCII "GLAI") so its own hook can ignore it. Windows
  then sees "Win + another key" and doesn't open Start when Win is released. The log string "Win/Alt
  mask key injection failed" shows the same trick protects Alt (stops menu-bar activation).
- No Start menu appeared in any run.

**Other Win shortcuts (inferred).** Keys are swallowed only once a binding is matched and only
for the keys belonging to it. Win+E, Win+V and so on pass through untouched because Ctrl isn't
held. Glaido also handles AltGr's fake Left-Ctrl ("Fake AltGr Left Ctrl").

**Other hotkey facts (observed).**

- **Esc** cancels the overlay ("Overlay: Escape pressed; dismissing").
- In hands-free mode, pressing the hold chord (Ctrl+Win) again **stops** the session. The window
  hid 86 ms after the Win key-down.
- Injected input is accepted, so Glaido doesn't filter `LLKHF_INJECTED`.
- Hotkey state is reset on screen lock and before updates.

### 4b. Audio capture, VAD, STT

- **Capture (observed).** WASAPI shared mode through cpal at the device's native rate. Here that
  is the Shure MV7: 48 kHz, 2 channels, f32, with speech channels [0,1] mixed down. The device
  handle is cached ("cache_hit=true"), so a new stream starts in about 25–30 ms. The first audio
  callback arrives 47–108 ms after the hotkey. A mic-monitor stream is pre-warmed at startup.
  "Auto" microphone mode picks the device by name. Bluetooth mics are excluded by default (the
  Bluetooth hands-free profile wrecks quality).
- **Denoise (observed).** RNNoise (nnnoiseless) runs on every recording.
- **VAD (observed/inferred).** Log `leading_silence_threshold_ms=750` for recordings. A
  `too_short` terminal code is used for silent or near-empty clips (every silent test ended
  `terminal_code=too_short … history=not_saved`). `enable_vad` is a field sent to the server, so
  the main voice-activity detection is server-side (inferred).
- **STT is cloud (observed).** Opus frames go over a WebSocket to `api.glaido.com`. The protocol
  has `operation_id`, `attempt_id`, resume-from-sequence, replay and HTTP retry. Providers named
  in errors and telemetry: `groq`, `baseten` (`baseten_connect_timeout`, `gpu_inference_ms`,
  `queue_wait_total_ms`). A second pass with an LLM clean-up exists: the "Raw text" style is
  described as "Exactly what you said, with no AI cleanup".
- **Biasing (observed field names).** Each request carries `dictionary`, `snippets`, `formatting`
  (rules with style and custom prompt, per app or site), `corrections`, `active_app_name`,
  `appCategory`, `siteHost`/`pageUrl`, `selected_text`, `language_options` and `audio_language`.
  The server biases transcription and clean-up with your dictionary and app context.
- **Limits (observed).** A recording is capped at **10 minutes**, with a "limit approaching"
  warning, then auto-stop and save. A separate `processing_limit` watchdog exists.
- **Mute background (observed).** An optional setting mutes the default output device with
  WASAPI `SetMute(true)` while recording and restores it afterwards.

### 4c. Paste and clipboard restore

**Observed from strings; inferred where noted.** Glaido pastes through the clipboard:

1. It snapshots the current clipboard. There is a size cap: "Clipboard snapshot unavailable or
   exceeded save cap; transcript remains in clipboard".
2. It puts the transcript on the clipboard and injects the paste keystroke with `SendInput`.
   **Terminals are detected by process name** (windowsterminal, openconsole, conhost, wezterm,
   alacritty, mintty, tabby, hyper, warp, conemu, putty, kitty, mobaxterm, termius, securecrt,
   xshell, ttermpro). They get a terminal-safe paste chord. The exact chord, probably
   Ctrl+Shift+V or Shift+Insert, is inferred.
3. It does a **deferred restore** of the original clipboard. The restore is skipped if the
   clipboard sequence number shows someone else changed it in the meantime.
4. Elevated target apps can't receive the paste (UIPI). Glaido shows "Glaido cannot paste into
   apps that run as administrator". If no text field is focused, it shows "Nowhere to paste" and
   keeps the text in History.

**Optional settings.** "Copy to clipboard" leaves the transcript on the clipboard as well.
**Enter to stop and paste** (hands-free only, off here): while dictating hands-free, Enter ends
the session and pastes. Inferred: the Enter is swallowed by the hook so it doesn't also reach the
app. In Commands mode, selected text is captured by injecting **Ctrl+C**, with clipboard
save and restore. It is skipped while physical modifiers are held and for terminals.

### 4d. Dictionary and learning from corrections

- **Dictionary (observed).** Each entry has a correct spelling plus optional misspellings (table
  `dictionary(word, predecessor…)`; misspellings JSON). The free plan has a word limit
  ("Your dictionary holds up to {limit} words"). Words are synced to the backend and sent with
  each request. Glaido can import from Wispr Flow.
- **Learning (observed: macOS only).** A `correction_candidates` table holds `wrong_word`,
  `correct_word`, `occurrence_count`, `wrong_variants_json`, `first_seen_at`, `last_seen_at` and
  `suggested_at`. The capture code is in `src/platform/macos/correction_capture.rs`. It uses the
  macOS Accessibility API to re-read the field after a paste and notice what the user changed.
  When a correction repeats, Glaido emits `correction-learning:suggestion` and shows a toast:
  *"You often correct "{word}". Add to dictionary?"* with an **Add** button. It skips words
  already in the dictionary and prunes stale candidates.
- **Windows: no equivalent file exists** (no `platform/windows/correction_capture.rs`, no UI
  Automation strings). On Windows, Glaido almost certainly does **not** learn from post-paste
  edits (inferred). The Dictionary dialog also shows "From your recent dictations" suggestions
  (`suggest_dictionary_misspellings`). Their Windows source is unknown.

### 4e. Stats definitions (observed: front-end code plus live `get_analytics` numbers)

The formulas check out exactly against the owner's data (1,546 words; 682,530 ms of speech).

- **Typing baseline = 52 wpm** (a hard-coded constant).
- **Time saved (minutes)** = `words / 52 − speech_minutes`, floored at 0. `speech_minutes` is
  `speech_total_ms / 60000`, falling back to `non_empty_total_ms`. Here 1546/52 − 11.38 = **18 min**.
- **Time-saved caption.** Fixed buckets: < 1 min "a tiny moment", < 5 "a quick task", < 15 "a
  short break", < 30 "a coffee break", < 60 "a lunch break", < 180 "watching a movie", < 480 "a
  full workday", then day, weekend, week, month, "a major life project".
- **WPM** = words ÷ speech minutes. It is computed in Rust (`productivity.words_per_minute`);
  here 1546 / 11.375 = 136.
- **"N× faster than typing"** = `round(wpm / 52, 1)`. Here 2.6×.
- **Day streak.** `engagement.current_day_streak` from Rust, along with `best_day_streak`,
  `active_days_*` and `week_streak`. Inferred: consecutive local calendar days with at least one
  saved dictation. Glaido carries a time-zone module, so "local day" is deliberate. The exact
  reset rule (for example, whether today must be active) is **unknown**. The caption follows
  milestones: ≥1 "Great start!", 3 "Building momentum", 5 "Full work week", 7 "A whole week",
  14, 21 "Habit formed", 30, 60, 90 "Unstoppable".
- **Insights page.** Has milestones (1k to 1M words, 1 to 160 hours saved, active days, commands),
  a 12-week WPM chart, a GitHub-style activity grid, and a server-side "Glaido rank" (percentile
  by words this month).

### 4f. History storage and retention

- **Format (observed).** SQLCipher database tables:
  - `sessions(id, mode, status, app_bundle_id, app_name, app_icon_path, selected_text, created_at, updated_at)`
  - `entries(id, session_id, role, content, raw_content, audio_path, is_error, created_at, context_json)`

  Both the cleaned `content` and the `raw_content` are kept. App icons are cached as PNGs in
  `app-icons/`. Audio is kept per recording as `audio/history/<local-timestamp>_<uuid>.wav.enc`
  (encrypted). The UI states: "Your transcriptions are encrypted and only stored locally on this
  device."
- **Failure safety (observed).** If transcription fails, the audio is preserved and History
  offers **Retry** (re-transcribes the saved audio). `audio_preserved=true` was logged even for
  `too_short` clips.
- **Retention: unknown.** No retention constant or setting was found. History has a manual
  "Clear history". All 30 audio files are from today because the app is new. Clipboard history
  does have a prune routine.

---

## 3. Popup spec (the recording bar)

Reference images:

- `docs/research/glaido/popup-states-4x-dark.png` and `…-light.png`: every state at 4× on
  transparent, composited on dark and light backgrounds.
- `popup-states-1x.png`: the same states at actual size.
- `popup-on-screen-1x.png` / `-2x.png`: the bar in place over the taskbar.
- `popup-recording-sequence-8x.png`: a real hold over time.

### 3.1 Window

| Property | Value | Evidence |
|---|---|---|
| Window size | **148 × 52 px** (at 100 % scaling) | observed (`GetWindowRect`, `innerWidth/Height`) |
| Visible pill | **116 × 38 px** (hold) / **134 × 38 px** (hands-free, with close ×) | observed (DOM) |
| Pill placement | Centred in the window (7 px of transparent margin above and below, 16 px left and right for the shadow) | observed |
| Horizontal position | Centred on the active monitor. Window x = 886 on a 1920-wide screen, so the centre is 960 | observed |
| Vertical position | Window bottom 26 px above the **monitor** bottom; pill bottom **33 px** above the monitor bottom (y 1009–1047 on 1080) | observed |
| Taskbar | The pill **overlaps the taskbar by 15 px** (taskbar top y = 1032). Glaido positions against monitor bounds, not the work area | observed position; cause inferred |
| Setting | "Dictation bar location": Bottom / Top, plus `floating_controls_vertical_offset` | observed |
| Window styles | Tool window, **no-activate**, topmost, transparent web background, not click-through. Pre-created hidden at startup and shown/hidden on demand | observed (exstyle 0x08000198) |
| Show latency | Visible **12–36 ms** after the chord completes | observed (5 runs) |
| Enter/exit | **Hard cut.** No fade, slide or scale. The pill appears fully formed and vanishes in one frame | observed (30 ms screen sampling); the overlay CSS has no entry animation |
| Hide | Immediate on release when the clip is too short. With real speech the bar stays in **processing** until the transcript arrives | observed (silent runs); processing duration with speech not measured |

### 3.2 Pill anatomy (left to right)

`[12 px pad] [logo 18×18] [8 px gap] [10 bars: 3 px wide, 4 px gap = 66 px] [12 px pad]`
= **116 px**.

Hands-free: the right padding drops to 0 and a close button is appended
(`[2 px margin] [20×20 button, 2 px padding, 14 px × icon] [8 px margin]`) = **134 px**.

| Token | Value |
|---|---|
| Height | 38 px |
| Corner radius | **12 px** (`--radius-overlay-floating`). A "pill" variant with 19 px radius exists but isn't used |
| Background | `#0d0d0d` (neutral-950), solid. The glass variant is off on Windows |
| Edge and shadow ("lit rim") | `inset 0 1px 0 rgba(255,255,255,.10)` (top highlight), `inset 0 0 0 1px rgba(255,255,255,.05)` (hairline), `0 1px 2px rgba(0,0,0,.30)`, `0 2px 6px -1px rgba(0,0,0,.50)` |
| Glass variant (unused) | Background at 72 % mix, `backdrop-filter: blur(18px) saturate(160%)`, 10 % white hairline |
| Logo / bars colour | `#bff549` (green-500) for dictation **and** command mode; also the warm-up colour on this overlay |
| Close × | `rgba(255,255,255,.7)`, white on hover |
| Command mode | The logo is swapped for a "»" arrow mark; colours unchanged |

### 3.3 States

| State | Look | Trigger |
|---|---|---|
| Hidden | Window hidden | idle |
| Warm-up / silent | 10 static **3 px dots** | Recording started, no audio yet (`warmup_marked` → `first_audio_marked`) |
| Recording | Bars dance between 3 and 18 px with the input level | `amplitude-update` events |
| Hands-free | As recording, plus the close × (pill 134 px) | `persistent-mode: true` |
| Processing | Bars frozen at 3 px plus a **shimmer**: a 160 px-wide gradient mask sweeping left to right every **1.6 s** (linear, infinite), dots dimmed to 30 % outside the band | `overlay-visual-transition {phase: processing}` |
| Command | Arrow mark replaces the logo | `mode: command` |
| Error / notices | Not drawn in the bar. Errors go to the **toast window** (552 × 120, bottom-right, 16 px from the right and bottom edges, click-through when idle) | observed |
| Reduced motion | Shimmer disabled | observed (CSS) |

### 3.4 Level meter: how audio level becomes bars (observed, front-end code)

1. The core emits `amplitude-update` (a single number per update). The overlay scales it:
   `level = min(1.5 × amplitude, 100)`.
2. Each bar runs its own `requestAnimationFrame` loop:
   - **Gate:** `target = level < 5 ? 0 : (level − 5) / 95` (clamped 0–1).
   - **Smoothing:** `s += (target − s) × k`, with **k = 0.35 rising, 0.12 falling** (fast
     attack, slow release).
   - **Curve:** `f = s^1.3`.
   - **Per-bar motion** (t in ms, i = bar index 0–9):
     `m = (sin(0.002t + 1.2i) + 0.4·sin(0.003t − 0.6i) + 1.4) / 2.8`.
   - **Idle floor:** `g = 3 + 1.5 × (0.5·sin(0.0015t + 0.6i) + 0.5)`, giving a 3–4.5 px breathing
     floor.
   - **Height:** `max(g, 3 + (18 − 3) × f × (0.15 + 0.85·m))` px.
3. All 10 bars share **one** level value. Their differences come only from the sine phases, so
   the meter is a styled loudness animation, not a spectrum.
4. Warm-up or processing forces 3 px.

Measured heights at fake levels 10 / 30 / 60 / 100: about 3–4.5 / 4–5.7 / 4.7–13 / 7–16 px.
Even at full level the bars rarely reach 18 px, because of the 0.15–1.0 motion multiplier and the
1.3 curve.

---

## 4. Main window layout spec

Reference images (captured at 1280 × 800 and 1280 × 1200 viewports; personal transcript text was
replaced with placeholder text before capture):

- `main-home-1280x800.png`, `main-formatting-…`, `main-dictionary-…`, `main-snippets-…`,
  `main-commands-…`
- `main-settings-1280x800.png` (Dictation tab), `main-settings-hotkeys.png`,
  `main-settings-general.png`, `main-settings-account.png`

### 4.1 Structure

- **One dark canvas**, `#0d0d0d`. Frameless window with a custom title area (about 48 px top band
  holding only "Go Pro" at the right).
- **Sidebar, 256 px wide, same colour as the canvas** (no divider):
  - Nav buttons 224 × 40 at x = 16, radius 4, padding 10, icon 16–20 px plus label 14/20.
  - Pitch 42 px. Home sits alone, then a 14 px gap before Formatting, Dictionary, Snippets,
    Commands (with a "Beta" chip: lime text on lime 15 %, 9–11 px).
  - Active item: background `rgba(255,255,255,.03)`, **icon turns lime `#bff549`**, label white.
    Inactive: label at 70 % white.
  - Footer: Settings, "Get a free month", and an account button (224 × 42, background
    `rgba(0,0,0,.1)`, 1 px `rgba(255,255,255,.1)` border, radius 4, avatar plus name plus chevron).
- **Content column** starts at x = 272 (16 px gutter), 16 px right gutter, top 48. Cards are
  `#1a1a1a`, **radius 8**, 16 px apart, with no borders or shadows. Depth comes only from the
  1a1a1a-on-0d0d0d step.
- **Home:**
  - Three stat cards, 320 × 179 each (container-query sized, padding `clamp(16px,3cqw,32px)`).
    Each has a lime icon plus a mono uppercase tag ("TIME SAVED"), a big value (Aspekta 500,
    `clamp(22px,3.5cqw,36px)`, letter-spacing .48 px, about 35 px here) and a 13/16 caption at
    70 %.
  - Below them, the **Activity** card (full width, padding 32/0/32/32):
    - Header: mono tag plus a shield "privacy" tooltip icon, and a search field (189 × 28,
      background white 3 %, radius 4).
    - Sticky date header (36 px): "Today" 15 px white plus the date 15 px at 70 %.
    - **Rows 72 px tall** (padding 16/12, radius 4): 40 × 40 app icon, transcript 14/20 white on
      one truncated line, app name 14/20 at 70 %, time 13/16 at 70 % right-aligned. A copy button
      (24 × 28) appears on hover.
- **Other pages** (Formatting / Dictionary / Snippets / Commands):
  - A **hero card** (992 × 116, padding 32): title 17/24 weight 500 plus description 14/20 at
    70 % with an underlined "the docs" link.
  - Then a **section card**: mono tag top-left, and a secondary button top-right ("+ Add word":
    28 px tall, radius 2, 1 px white 10 % border, padding 4/10).
  - Empty state: 48 × 48 icon tile (radius 6, white 3 %), title 17/500, two-line description
    14/20 at 70 %, and a lime primary button (28 px, radius 2, black 14/500 text).
  - Commands uses a segmented control (320 × 36, white 5 %, radius 6; active segment white 10 %)
    and a two-column grid of tool cards (458 × 70, white 5 %, 1 px white 10 % border, radius 8,
    36 × 36 icon tile, 14/500 title, 13/16 subtitle, toggle on the right).
- **Settings is a modal**, not a page:
  - Size `90vw × 85vh`, max **992 × 742**, radius 12, 1 px white 10 % border, background white
    5 % with `backdrop-filter: blur(200px)`, padding 24, over a black scrim.
  - Title 20/28 weight 500.
  - Left tab rail about 244 wide: Dictation, Hotkeys, General, Account (40 px items, same style as
    the sidebar), with the version number at the bottom.
  - Right side: page title 17/24 weight 500, then mono uppercase section tags (INPUT, LANGUAGE,
    BEHAVIOR…).
  - **Setting rows:** 40 × 40 icon tile (radius 4, white 3 %), title 14/500 white, description
    13/16 at 70 %, and a control on the right.
  - Controls:
    - **Toggle** 40 × 20 (on `#8cc21f`, off `#333`).
    - **Select** 28 px tall.
    - **Key chips** (`kbd`, Geist Mono 12, background white 10 %, radius 2, padding 4/8, 24 px
      tall, 6 px apart) next to a pencil "edit" icon.
    - Danger action: red `#ff2b3a` button ("Clear history").

### 4.2 Tokens

**Colour (dark theme, the one in use):**

| Role | Value |
|---|---|
| Canvas | `#0d0d0d` |
| Card | `#1a1a1a` |
| Menu | `#333` |
| Text | `#fff` / 70 % `#ffffffb3` / 30 % (disabled) |
| Hairline border | white 10 % |
| Hover fills | white 3 % / 5 % / 10 % |
| Accent | lime `#bff549` (hover `#8cc21f`, dark `#6fa014`). Toggles use `#8cc21f` |
| Errors | `#ef4444` / `#ff2b3a` |
| Info | `#3b82f6` |
| Warning | `#f59e0b` |

Light theme flips the neutrals (canvas `#f2f2f2`, cards white, text black) and **swaps the accent
from lime to violet `#4b3fc4`**.

**Type:**

- Families: **Aspekta** (400/500/600) for UI and **Geist Mono** for tags, keys and code.
- Scale (letter-spacing 0.01 em throughout):

| Style | Size / line-height | Weight |
|---|---|---|
| display-lg | 48 / 48 | 500 |
| display-md | 32 / 32 | 500 |
| display-sm | 24 / 24 | 500 |
| heading-lg | 20 / 28 | 500 |
| heading-md | 17 / 24 | 500 |
| heading-sm | 15 / 20 | 400 |
| body-md | 14 / 20 | 400 or 500 |
| body-sm | 13 / 16 | 400 or 500 |
| body-xs | 11 / 14 | 400 |
| code | mono 12 / 16 | — |
| tag | mono 12 / 14, uppercase | — |

**Radius:** 2 (buttons, inputs, chips) / 4 (nav, rows, icon tiles) / 6 / 8 (cards) / 12 (modal,
overlay) / full.

**Spacing scale:** 2, 4, 6, 8, 10, 12, 16, 20, 24, 32, 40, 56, 80, 120.

**Shadows:** almost none in the main window. `--shadow-elevation: 0 4px 24px #00000080` for
floating things; glow `0 0 6px #bff54980`.

**Motion:**

| Token / element | Value |
|---|---|
| `--motion-1..4` | 120 / 180 / 240 / 320 ms |
| ease-enter | `cubic-bezier(.165,.84,.44,1)` |
| ease-enter-far | `cubic-bezier(.16,1,.3,1)` |
| ease-exit | `cubic-bezier(.3,0,.8,.15)` |
| ease-move | `cubic-bezier(.65,0,.35,1)` |
| ease-sheet | `cubic-bezier(.32,.72,0,1)` |
| ease-settle | spring-like `linear()` that overshoots to 1.028 |
| Dialog in | 240 ms ease-enter-far, from opacity 0, scale .96, y +8 px |
| Dialog out | 180 ms ease-exit, to scale .98, y +4 px |
| Scrim | fades 240 / 180 ms |
| Popovers / selects | 180 ms |
| Sheets | 320 ms ease-sheet from y +24 px, scale .97 |
| Hover colour changes | 150 ms `cubic-bezier(.4,0,.2,1)` |
| Scrollbars | 6 px, thumb white 3 % → 10 % on hover/active |

**Other windows:** a "Command" modal window (576 × 32 at top centre, pre-created) and the toast
window (552 × 120, bottom-right).

---

## 5. Design takeaways for Wren

1. **Keep the bar's footprint** (Maximilian likes it): about 116 × 38, radius 12, a near-black
   solid fill and a 1 px lit rim. Fix what's weak:
   - **Respect the work area.** Glaido's bar sits 15 px over the taskbar. Position against
     `SystemParametersInfo(SPI_GETWORKAREA)` (or `MonitorFromPoint` + `GetMonitorInfo.rcWork`)
     with an 8–12 px gap above the taskbar.
   - **Animate in and out.** Glaido hard-cuts. Use a 120–180 ms fade + 4–6 px rise + .96→1 scale
     in, and an 120 ms fade + .98 out (Glaido's own dialog tokens are a fine model). Animate the
     width change when hands-free adds the close button instead of snapping 116→134.
   - **Make the meter mean something.** Glaido drives all 10 bars from one loudness value plus
     sine noise, so it looks alive even when it isn't listening well. Wren could feed a few FFT
     band energies (or a short history of RMS values scrolled across the bars). The bars would
     then react to the voice, while keeping the good parts: the noise gate at 5 %, fast attack
     (.35) and slow release (.12), the ^1.3 curve, and a 3 px idle floor with gentle breathing.
   - **Distinct states.** Glaido's warm-up, recording and command states all use the same lime.
     Give warm-up a muted grey dot row, recording the accent, hands-free a visible "locked"
     cue, and processing the shimmer (a 1.6 s sweep reads well; keep it). Respect reduced-motion.
   - **Show errors at the bar.** A brief inline state ("No speech", "Offline, saved") before
     handing off to a toast. Glaido silently hides on `too_short`, which feels like nothing
     happened.
2. **Hotkey engine.** Copy the *approach*, not the code:
   - A WH_KEYBOARD_LL hook on its own thread with a message pump.
   - Swallow only the keys of a matched chord.
   - When Win or Alt has already reached the OS, inject an unassigned VK (0xFF) tagged with your
     own `dwExtraInfo` so Start or the menu bar doesn't fire.
   - Start recording on the first matching chord and *promote* to hands-free if Shift arrives.
     This gives zero-latency starts.
   - Esc cancels.
   - Reset state on screen lock.
3. **Paste.**
   - Clipboard snapshot → set text → `SendInput` Ctrl+V (a terminal-specific chord for known
     terminal processes) → deferred restore guarded by `GetClipboardSequenceNumber`.
   - Detect elevated targets and say so, rather than failing silently.
   - Always save to History first, so a failed paste loses nothing.
4. **Audio.**
   - Pre-warm and cache the device so first audio arrives in under 60 ms.
   - Run RNNoise or an equivalent before STT.
   - Keep the audio until the transcript is confirmed, so "Retry" works offline.
   - 10-minute cap with a warning.
5. **Learning from corrections** is a real gap on Windows; Glaido only does it on macOS. Wren can
   do it with UI Automation: after pasting, keep the focused element's `TextPattern` or
   `ValuePattern` reference for about 30 s. Diff what the user changed against what was pasted.
   Count word-level substitutions, and after 2–3 repeats offer "Add X to dictionary?".
6. **Stats.** Use honest, explained formulas. Glaido's are simple and checkable: a typing baseline
   of 52 wpm, time saved = words/52 − speaking minutes, WPM = words ÷ speaking minutes. Wren
   should show the formula in a tooltip and let the user set their own typing speed.
7. **Main window.** Glaido's calm structure works: a sidebar the same colour as the canvas,
   `#1a1a1a` cards with radius 8, mono uppercase section tags, a single accent used sparingly,
   and a settings modal with a left tab rail. Improvements for Wren:
   - Give the History rows more information density. Two lines of text, the duration and WPM
     per row are more useful than a 72 px row with one truncated line.
   - Put settings on a page rather than a modal, so they have room to grow.
   - Have a light theme that is a first-class design, not an inversion.

---

## 6. Checklist status

| # | Question | Status | Notes |
|---|---|---|---|
| 1 | Stack: crates, Tauri version, imports, embedded assets | **answered** | No cargo-auditable section; crates recovered from source paths. 110 assets extracted |
| 2 | Main window pages: screenshots plus tokens | **answered** | Home, Formatting, Dictionary, Snippets, Commands, Settings (4 tabs). Not captured: Insights page (no nav entry visible), referral modal, profile menu, history detail |
| 3 | Popup size, position, colours, states, animation, level mapping | **answered** | Processing state was rendered by replaying Glaido's own front-end events (silence never reaches processing). Error states live in the toast window, not the bar |
| 4a | Hotkey hook, hold vs double-tap, Start suppression, other Win shortcuts | **partial** | Mechanism and suppression observed. Double-tap and grace **ms values unknown** |
| 4b | Capture, VAD, local vs cloud, biasing | **answered** | Cloud (Groq/Baseten via api.glaido.com). Server-side VAD inferred |
| 4c | Paste and clipboard restore, Enter-to-paste | **partial** | Flow from strings. Exact terminal paste chord inferred. Enter-to-paste not exercised (setting is off) |
| 4d | Dictionary and correction learning | **partial** | Learning observed as **macOS-only**; Windows source of "recent dictation" suggestions unknown |
| 4e | Time saved, WPM, streak | **partial** | Time saved and WPM exact. Streak reset rule unknown |
| 4f | History format and retention | **partial** | Schema and encryption answered. **Retention unknown** |
| 5 | Relaunch Glaido normally | **done** | Relaunched 03:16 without the debug variable. Main window minimised as found. CDP port closed |

## 7. Side effects of this investigation

- Five silent test recordings (about 2–6 s each) went to Glaido's servers. Each ended `too_short`;
  none was pasted or added to the History list. Glaido may have kept their (silent, encrypted)
  audio in `audio/history/`. Nothing was deleted.
- Glaido was restarted three times. The first debug instance died at 02:07:18 UTC for an
  unknown reason. It was relaunched from the Start menu at 02:07:23 UTC by physical keystrokes,
  which means someone else was at the machine or another session acted. It was then restarted
  with the debug port and finally restarted normally.
- Notepad was opened for the tests. Its restored tabs include files named `*.key` and `*.env`.
  Tests ran in a fresh blank tab, which was then closed. Notepad has been closed. Full-screen
  captures were deleted from the cache.
- No settings were changed, no sign-in or sign-out happened, and nothing was purchased.
