# Wren — build spec (v1)

Date: 2026-11-02. Owner: Maximilian. Inputs: `concept-input.md` (what he asked for),
`goldfish-brainstorm-report.md` (six-lens review; its defaults are adopted unless overridden here),
`../research/willow-teardown.md`, `../research/glaido-teardown.md`, `../research/stt-benchmark.md`.

Wren is a free, local, single-user Windows 11 dictation app. Hold **Left Ctrl + Left Win**, talk, let go:
the words are pasted into the focused app (98% of the time the Claude desktop app). Double-tap the same
chord for hands-free. Everything runs on this PC; nothing is sent anywhere.

Where this spec and the Goldfish report disagree, **this spec wins**.

---

## 1. Architecture

```
┌───────────────────────── Wren.exe (Tauri 2, Rust) ─────────────────────────┐
│ hotkey (LL hook thread + pure chord machine)                                │
│        │ events                                                             │
│        ▼                                                                    │
│ session (sole owner of dictation state, dictation IDs, ordering)            │
│   ├─ audio   (cpal capture → 16 kHz mono WAV + ~30 Hz level events)         │
│   ├─ engine  (supervises the speech sidecar, JSON lines over stdio)  ◄──┐   │
│   ├─ pipeline (pure text steps: guards, fillers, "i", replacements)     │   │
│   ├─ paste   (clipboard save → set → SendInput Ctrl+V → restore)        │   │
│   ├─ store   (rusqlite: transcriptions, dictionary, hourly totals)      │   │
│   └─ watcher (UI Automation correction watcher, optional)               │   │
│ ui: main window (React) + popup window (React, never focused)           │   │
│ tray, single instance, logs, --selftest                                 │   │
└─────────────────────────────────────────────────────────────────────────┼───┘
                                                                          │
               wren-sidecar (Python 3.12 venv, faster-whisper on CUDA) ───┘
```

- **No module calls another sideways.** Everything flows through `session` or Tauri events.
- Rust crate layout: `src-tauri/src/{main.rs, lib.rs, constants.rs, paths.rs, settings.rs, logging.rs,
  hotkey/{mod.rs, hook.rs, chord.rs}, audio.rs, engine/{mod.rs, protocol.rs, supervisor.rs},
  pipeline/{mod.rs, tidy.rs, guards.rs, replace.rs}, paste/{mod.rs, clipboard.rs}, store/{mod.rs,
  schema.rs, stats.rs}, dictionary/{mod.rs, learn.rs}, watcher/{mod.rs, uia.rs}, session.rs, popup.rs,
  tray.rs, commands.rs, selftest.rs}`.
- Front end: `src/` (React 18 + TypeScript + Vite), two entry points: `index.html` (main window) and
  `popup.html` (popup window). Shared design tokens in `src/styles/tokens.css`.
- Sidecar: `sidecar/` (Python package `wren_sidecar`, `pyproject.toml`, `uv.lock`). Shipped as a Tauri
  resource; its venv lives in `%LOCALAPPDATA%\Wren\runtime\.venv`.
- Reference code: Handy (MIT) is checked out at `C:\Wren\.research-cache\handy`. Builders may read it and
  adapt patterns (overlay, clipboard, paste, engine supervisor, recorder). Any file substantially derived
  from Handy keeps an MIT notice comment ("Adapted from Handy, MIT, Copyright (c) 2025 CJ Pais").
  Never copy Willow or Glaido code.

## 2. Shared constants (single source: `src-tauri/src/constants.rs`; mirrored read-only to the UI)

| Name | Value | Meaning |
| --- | --- | --- |
| `VK_CHORD_CTRL` / `VK_CHORD_WIN` | 0xA2 / 0x5B | Left Ctrl / Left Win |
| `VK_MASK` | 0xE8 | Unassigned key injected to stop Start opening |
| `INJECT_TAG` | 0x5752454E ("WREN") | `dwExtraInfo` on every key Wren injects |
| `TAP_MAX_MS` | 400 | Chord released before this = tap (audio discarded) — Willow's value, which he is used to |
| `DOUBLE_TAP_GAP_MS` | 600 | 2nd chord-down within this of the 1st release = hands-free (Willow) |
| `HANDSFREE_STOP_DEBOUNCE_MS` | 500 | Ignore chord presses this long after hands-free starts |
| `POPUP_DELAY_MS` | 90 | Popup appears this long after chord-down (no flash on Ctrl+Win+→) |
| `KEYUP_WAIT_MAX_MS` | 2000 | Max wait for both chord keys up before pasting |
| `CLIPBOARD_RESTORE_MS` | 500 | Restore previous clipboard this long after the target read our text |
| `MIN_SPEECH_MS` | 300 | Less speech than this (VAD) = nothing pasted |
| `SHORT_CLIP_MS` | 2000 | Under this, drop known silence hallucinations |
| `MAX_RECORDING_MS` | 600000 | 10 min cap (hold and hands-free); warning at 540000 |
| `HANDSFREE_SILENCE_STOP_MS` | 90000 | Continuous silence → stop, save, don't paste |
| `KEYSTATE_POLL_MS` | 250 | While recording, check physical key state (lost key-ups) |
| `RETENTION_HOURS` | 72 | Transcriptions + audio deleted after this |
| `TYPING_WPM_DEFAULT` | 52 | Baseline for "time saved" (setting) — Glaido's value, matches the numbers he has seen |
| `BIAS_TERMS_MAX` | 50 | Dictionary terms sent to the model per dictation |
| `WATCH_POLL_MS` / `WATCH_MAX_MS` | 500 (2000 after 10 s quiet) / 300000 | Correction watcher (Willow's cadence) |
| `SIDECAR_RESTART_BACKOFF_MS` | [1000, 2000, 5000] | then error after 5 failures in 5 min |

## 3. Hotkey (the part the app lives or dies on)

- A `WH_KEYBOARD_LL` hook installed by Rust on its **own thread with a message loop**. The callback only
  classifies the event, updates the pure chord machine and posts a message to the session thread. No I/O,
  no logging, no locks held across calls. **Every event is passed on with `CallNextHookEx`** — Wren never
  swallows Ctrl, Win or any other key.
- Events carrying `dwExtraInfo == INJECT_TAG` are ignored by the machine (Wren's own mask key and Ctrl+V).
  Other injected keys are accepted (so tests can drive the hotkey).
- **Chord machine** (`hotkey/chord.rs`, pure, table-tested, no OS calls). Inputs: key down/up with VK,
  injected flag, timestamp. Outputs: `HoldStart`, `HoldEnd`, `Tap`, `HandsFreeStart`, `HandsFreeStop`,
  `Cancel`, `MaskNow`.
  - Chord = Left Ctrl + Left Win down, either order, no other key down. Auto-repeat downs ignored.
    Right-hand Ctrl/Win never count.
  - On chord-down: emit `MaskNow` (session injects `VK_MASK` down+up tagged, while Win is still held) and
    `HoldStart` (session starts audio immediately so the first word is never clipped).
  - Any other key down while the chord is held (Shift, Alt, letters, arrows, Enter…): emit `Cancel`
    (discard audio, hide popup, let the Windows shortcut work). Exception: if recording has run ≥ 3 s, the
    audio is transcribed and saved to History but not pasted.
  - First chord key up: if held < `TAP_MAX_MS` → `Tap` (discard), and open a `DOUBLE_TAP_GAP_MS` window;
    a chord-down inside that window → `HandsFreeStart`. If held ≥ `TAP_MAX_MS` → `HoldEnd` (transcribe).
  - Hands-free: other keys never cancel (he may type). The next chord press (after the debounce) →
    `HandsFreeStop`. Also stop on `MAX_RECORDING_MS`, `HANDSFREE_SILENCE_STOP_MS`, lock or sleep.
- **Start-menu mask:** inject `VK_MASK` at chord recognition while Win is held, for every chord press.
  Builders must verify with an automated loop (inject the chord 50 times, check
  `StartMenuExperienceHost`/`SearchHost` never becomes foreground). If at-chord fails, also inject at the
  first key-up; keep whatever passes 50/50.
- **Watchdog:** reinstall the hook on resume from sleep and session unlock, and if `GetLastInputInfo` shows
  input but the hook saw nothing for 5 s. Tray item "Reinstall hotkey". While recording, poll
  `GetAsyncKeyState` every `KEYSTATE_POLL_MS` to recover lost key-ups.
- **Admin windows:** if an elevated window has focus the hook may not see keys and paste won't arrive.
  Accept; when paste can't be verified, the text is in History.

## 4. Audio

- `cpal` on WASAPI. Device opened at startup (Windows default input, following default changes; picker in
  settings), **stream started on chord-down**, stopped on end. Resample to 16 kHz mono f32 (`rubato`),
  write `%LOCALAPPDATA%\Wren\audio\<dictation_id>.wav` (16-bit PCM) with `hound`.
- Emit `level` events (RMS → 0..1, perceptual curve) at ~30 Hz to the popup.
- If no non-silent audio within 1.5 s of chord-down: popup shows "No sound from mic" (click opens
  `ms-settings:privacy-microphone`).
- Measure first-word clipping in the live test; if clipped, add an "always-open mic with 300 ms pre-roll"
  setting (off by default).
- **Test seam:** if the environment variable `WREN_TEST_AUDIO` names a WAV file, the audio module streams
  that file in real time instead of the mic (levels included). Only read at startup; logged loudly. Used by
  the live E2E test and `--selftest`; never set in normal use.

## 5. Speech engine (sidecar)

- Python 3.12 venv created by `uv` from `sidecar/pyproject.toml` + `uv.lock` into
  `%LOCALAPPDATA%\Wren\runtime\.venv`. CUDA 12 cuBLAS + cuDNN 9 from the `nvidia-*-cu12` wheels, added with
  `os.add_dll_directory` + PATH before importing faster-whisper (recipe: Overseer's `stt.py`; share no code
  or files with Overseer).
- Model files pre-downloaded to `%LOCALAPPDATA%\Wren\models` (never downloaded at app start).
  `local_files_only=True`.
- **Default model/settings:** taken from `../research/stt-benchmark.md` recommendation. If the benchmark is
  missing or inconclusive: faster-whisper `large-v3-turbo`, `int8_float16`, `language="en"`,
  `vad_filter=True`, `condition_on_previous_text=False`, beam 5 if a 10 s clip's release-to-text p50 ≤ 600 ms
  else 1. Model + settings live in `settings.json` (`engine.*`) so they can change without code changes.
- Bias (benchmark result, `../research/stt-benchmark.md`): `initial_prompt = "Glossary: t1, t2, …"` built from the
  ranked dictionary terms (≤ `BIAS_TERMS_MAX`, kept under ~200 tokens), plus the optional `prompt` sentence.
  **Do not use `hotwords`**: it made the model append dictionary words to trailing silence. The Glossary prompt
  raised name recall 80% → 93% with no harm elsewhere. VAD stays on (it removes the prompt-echo on silence).
- Launched by Rust with `CREATE_NO_WINDOW` inside a Job Object with `KILL_ON_JOB_CLOSE`. stderr → log file.
- Warm-up: load the model, transcribe a bundled 10 s WAV, then send `ready`.
- Supervision: restart with backoff; after 5 failures in 5 min → error state shown in popup and Settings.
- If CUDA fails: fall back to `small.en` int8 on CPU and **say so** (popup badge "CPU", Settings status).
- Pressed before ready: record anyway, popup shows "Warming up…"; transcribe when ready; paste only if within
  5 s of release and the same window is still focused, else save to History with a notice.

**Protocol v1 (JSON lines, one object per line, UTF-8):**

| Dir | Message |
| --- | --- |
| sidecar → app | `{"v":1,"type":"ready","engine":"faster-whisper","model":"…","device":"cuda","compute":"…","vram_mb":N,"caps":{"hotwords":false,"prompt":true,"vad":true}}` |
| app → sidecar | `{"v":1,"type":"transcribe","id":"…","audio_path":"…","bias_terms":["…"],"prompt":"…"}` |
| sidecar → app | `{"v":1,"type":"result","id":"…","text":"…","speech_ms":N,"infer_ms":N,"segments":[{"start":s,"end":s,"text":"…","no_speech_prob":p}]}` |
| sidecar → app | `{"v":1,"type":"error","id":"…|null","code":"OOM|BAD_AUDIO|NOT_READY|INTERNAL","msg":"…"}` |
| either | `{"v":1,"type":"ping"}` / `pong`, `{"v":1,"type":"shutdown"}` |
| sidecar → app | `{"v":1,"type":"log","level":"info|warn|error","msg":"…"}` (no dictated text) |

## 6. Text pipeline (pure Rust, each step has its own table tests)

1. **Silence guard:** speech < `MIN_SPEECH_MS` → empty. Clip < `SHORT_CLIP_MS` and text is a known
   hallucination ("Thank you.", "Thanks for watching!", "you", "Bye.", ".", the prompt text) → empty. Any
   phrase repeated > 4 times in a row → collapsed to one, flagged.
2. Whitespace normalise.
3. **Fillers** (whole words, case-insensitive except all-caps survives): um, umm, uh, uhh, er, err, erm,
   ermm, hmm, mm. Then remove orphaned commas, fix doubled spaces, re-capitalise sentence starts.
   Never removed: like, you know, I mean, so, well, basically, actually, ah, yeah.
4. `i`, `i'm`, `i've`, `i'll`, `i'd` → capitalised.
5. **Replacements** (active dictionary replacements, whole-word, case-aware, longest first). Record applied
   rule IDs.
6. [Optional AI clean-up — §10.]
7. Replacements again (in case AI changed spelling back).
8. Final spacing. **No trailing space, no newline.** Never paste empty text.

## 7. Paste

- Target = foreground window at stop (`GetForegroundWindow` + process name).
- Save to the store **before** pasting (so History always has it).
- Wait until both chord keys are physically up (max `KEYUP_WAIT_MAX_MS`), else don't paste.
- If the foreground window now differs from the target, or is Wren itself → don't paste; popup
  "Not pasted — click to copy" (4 s) and the text is copied to the clipboard instead.
- Clipboard: back up every clipboard format that can be copied as plain memory (`EnumClipboardFormats`;
  at least `CF_UNICODETEXT` and `CF_DIBV5`/`PNG`). Write Wren's text with **delayed rendering** from a hidden
  owner window (`SetClipboardData(CF_UNICODETEXT, NULL)` then render on `WM_RENDERFORMAT`), plus the
  registered formats `ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`=0,
  `CanUploadToCloudClipboard`=0. Send Ctrl-down, V-down, V-up, Ctrl-up in **one** `SendInput` call (tagged).
  The `WM_RENDERFORMAT` is the read receipt: poll every 10 ms for up to 1000 ms. If it never comes, try
  Ctrl+Shift+V once; if still unread → outcome `notPasted`, leave the text on the clipboard ("Copied — press
  Ctrl+V"). After a read, restore the backup `CLIPBOARD_RESTORE_MS` later, only if
  `GetClipboardSequenceNumber` shows nobody else wrote. Retry `OpenClipboard` with back-off (5 × 20 ms).
  (Technique observed in Willow; written fresh for Wren.)
- **Never type text key by key** (a newline would send the Claude message).
- Dictations finishing out of order are pasted in start order.

## 8. Dictionary and learning

- Entry kinds: **term** (bias the model) and **replacement** (`from → to`, whole word, case-aware).
- Sources: `willow_seed`, `manual`, `learned_history`, `learned_paste`. Status: `candidate`, `active`,
  `disabled`. Every entry counts `hits` and remembers `origin_transcription_id`.
- **No Willow import:** the owner's Willow `dictionary.json` holds only 4 text shortcuts (snippets, which he
  does not want), so there is nothing to seed. The `willow_seed` source stays in the schema, unused.
- **Fix a word (History):** select a word in a History card, type the right one → active replacement
  (`from` = heard, `to` = typed) + active term (`to`). Toast "Wren will write X from now on · Undo".
- **Correction diff (`dictionary/learn.rs`, pure, shared):** given pasted text and edited text, accept only
  1–3 word runs replaced by 1–3 words with unchanged neighbours, where old/new sound alike (Double Metaphone,
  `rphonetic`) or are spelt alike (normalised edit distance ≤ 0.4, `strsim`). Ignore case-only and
  punctuation-only edits, pure insertions/deletions, and more than 2 separate edits (a rewrite).
- **Auto-learned from the text box** (`learned_paste`): becomes an active **term** at once. Becomes an active
  **replacement** at once if `from` is not a common English word (top-20k frequency list bundled) and
  `len(from) > 2`; otherwise after 2 identical sightings. Toast "Learned: X → Y · Undo" (popup, 6 s).
- Bias list: active manual + learned terms (ranked by hits then recency), then seed terms, capped at 50.
- **Correction watcher (`watcher/`):** after a successful paste, read the focused element with UI
  Automation (TextPattern `DocumentRange.GetText`, fall back to ValuePattern), first at 250 ms, retry once
  after 300 ms if empty. Then poll every 500 ms while less than 10 s has passed since the paste or the last
  change, every 2000 ms after that, up to `WATCH_MAX_MS`, on its own thread. Stop when focus moves to another
  element, the box empties (message sent), the text stops resembling the dictation, 3 UIA failures, or the
  next dictation starts. Never watch terminals (`WindowsTerminal.exe`, `conhost.exe`, `cmd.exe`,
  `powershell.exe`, `pwsh.exe`). This is the method Willow uses (it then asks its cloud which words to keep;
  Wren decides locally with the diff below). Keep the last non-empty snapshot. Diff the pasted
  span (located by the text around it) with the learn function. Any UIA call taking > 250 ms counts as slow;
  three slow calls → watcher disables itself for that app and logs why.
- The watcher is **on by default only if** the read-only probe against the Claude desktop app (built and run
  tonight) shows its input box text is readable in < 50 ms; otherwise off, with the reason shown in Settings.
  Tests never type into the real Claude app.

## 9. Store

- `rusqlite` (bundled), owned by Rust. `%LOCALAPPDATA%\Wren\db\wren.db`, WAL, `secure_delete=ON`,
  migrations via `PRAGMA user_version`.
- Tables: `transcriptions(id TEXT PK, created_at_utc, local_date, local_hour, mode hold|handsfree,
  audio_path, duration_ms, speech_ms, raw_text, tidied_text, final_text, words, model, device,
  timings_json, target_process, paste_outcome pasted|not_pasted|copied|failed, applied_rule_ids)`;
  `dictionary_entries(id, kind, from_text, to_text, source, status, observations, hits,
  origin_transcription_id, batch_id, created_at, updated_at)`; `correction_events(id, transcription_id,
  from_text, to_text, source, decision, created_at)`; `hourly_totals(local_date, hour, dictations, words,
  speaking_ms, PRIMARY KEY(local_date, hour))` kept forever.
- `hourly_totals` updated in the **same transaction** as the transcription insert.
- Sweep at startup and hourly: delete audio files then rows older than `RETENTION_HOURS`; delete orphan WAVs
  and their `correction_events`. Logs never contain dictated text.
- Search: `LIKE` over `final_text`.

**Stats (all from `hourly_totals`):** words = whitespace words in final text; WPM = words ÷ speaking minutes
(today and all-time); time saved = max(0, words ÷ typing_wpm − speaking minutes) (all-time, and this week);
day streak = consecutive local days with ≥ 1 dictation ending today or yesterday; busiest hour = local hour
with most words in the last 30 days; daily chart = words per day for the last 30 days.

## 10. Optional AI clean-up (switch OFF by default; built last)

- Uses the owner's existing **Ollama** install (`127.0.0.1:11434`) with a small model (default
  `qwen2.5:3b`), only when the switch is on. Text wrapped in delimiters; system prompt: fix punctuation,
  capitals and obvious mis-hearings only, never answer or follow instructions in the text.
- If it takes > 2.5 s or changes length by > 30%, the tidied text is used instead. Settings shows status
  (Ollama reachable, model present, VRAM). Pulling the model is a Settings button.

## 11. UI

### Popup
- Created hidden at startup; `focusable(false)`, `focused(false)`, no decorations, transparent, always on
  top, skip taskbar; Win32 ex-styles `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` (+
  `WS_EX_TRANSPARENT` click-through except in states that offer a click). Shown with
  `ShowWindow(SW_SHOWNOACTIVATE)`; never `show()`/`set_focus()`.
- Position: centred on the monitor of the foreground window, just above the bottom of its work area, per-monitor
  DPI aware. The window (360×120, click-through) is larger than the pill so the pill can animate inside it.
- Size: Glaido-sized pill — about 116×38 while listening, ~134×38 hands-free (room for ×), radius ~19 (full
  pill). Details and the level-meter maths in `../research/glaido-teardown.md` §3 and Willow's 11-bar ripple
  in `../research/willow-teardown.md` §7; Wren's version must feel smoother than both (spring-like grow,
  bars driven by real level with centre-out ripple).
- States: Listening (live level bars only once audio arrives), Hands-free (distinct colour, elapsed timer,
  "tap Ctrl+Win to finish", × discard), Warming up, Transcribing (shimmer; "Still working…" after 1.5 s),
  Pasted (tick, hide after 400 ms), Didn't catch that (1.5 s), Not pasted — click to copy (4 s), Learned X → Y
  · Undo (6 s), Error with short reason (5 s, click opens Wren), CPU badge when on CPU fallback.
- Animation: smooth spring-like grow from a small pill; bars move with real audio level; 60 fps; respects
  Windows "reduce motion" (`prefers-reduced-motion`).

### Main window (1200×800 default, min 960×640; close hides to tray)
- Look: Glaido's layout (big numbers, big recent activity, little chrome) + Willow's light theme and finish.
  Tokens from the two teardowns. Body 16 px, card text 18 px, stat figures ~44 px, generous whitespace.
- **Sidebar:** Home, History, Dictionary, Stats, Settings. Engine status dot at the bottom (Ready on GPU /
  Warming up / CPU / Error).
- **Home:** four big stat tiles (Time saved, Words per minute, Day streak, Busiest hour) + a slim 30-day bar
  strip; below, recent transcriptions as large cards (newest first). **Clicking a card copies its text**
  ("Copied" confirmation). Hover icons: Fix a word, Delete.
- **History:** search box + all transcriptions in the last 72 h grouped by day; same cards; "Deleted after
  3 days" note.
- **Dictionary:** add word / add replacement; lists for Words (terms), Replacements, Learned (with source,
  hits, undo/delete).
- **Stats:** totals (all time, this week, today), 30-day daily chart, hour-of-day chart with busiest hour,
  WPM trend.
- **Settings:** microphone picker + live level test; correction watcher toggle (with probe verdict); AI
  clean-up toggle + status; typing speed for time saved; start with Windows (off by default); sounds (off);
  engine status (model, device, VRAM, restart engine); delete all history now; Willow/Glaido running
  warning; app version.
- **Tray:** Open Wren, Pause dictation, Reinstall hotkey, Quit. Single instance (`tauri-plugin-single-instance`).
- Startup banner if Willow or Glaido is running (double paste warning). Never kill or modify them.

## 12. Files and locations

- Settings: `%APPDATA%\Wren\settings.json` (versioned, atomic write).
- Data: `%LOCALAPPDATA%\Wren\{db,audio,models,runtime,logs}`. Logs: JSON lines via `tracing` +
  `tracing-appender`, one record per dictation with stage timings, no text.

## 13. Definition of done (acceptance)

Adopts §6 of the Goldfish report, plus:
- `cargo test` (chord machine, pipeline, learn diff, store/stats, protocol) and `pytest` (sidecar) green.
- `wren --selftest` passes: hook installs, mic present, sidecar ready on CUDA, DB writable, bundled WAV →
  text.
- Live E2E (only with session unlocked and Willow/Glaido closed): injected chord + fake-mic WAV input pastes
  the expected text into a **test window** (a WebView2/Electron page with a textarea and a contenteditable),
  popup never takes focus, Start never opens in 50 tries, Ctrl+Win+→ / Win+Shift+S / Win+V still work.
- Read-only UIA probe report on the Claude desktop app.
- UI screenshots of every page and every popup state saved to `docs/screenshots/`.
- NSIS per-user installer built and installed; Start-menu shortcut works.
