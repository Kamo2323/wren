# Willow Voice teardown (v2.2.2, Windows)

Static analysis only. Willow was never launched. Source: the installed app at
`C:\Users\maxha\AppData\Local\Programs\willow-voice` (`app.asar` extracted and
prettified into `C:\Wren\.research-cache\willow\`, git-ignored) plus a
read-only look at `C:\Users\maxha\AppData\Roaming\willow-voice`.
`supabase-session.json` was not opened. API endpoints and keys exist in the
code (Supabase, Vercel API, PostHog, Sentry, a WebSocket "middleware"); their
values are deliberately not recorded here. Wren will use none of them.

**How to read the tags**
- **Observed**: seen directly in code or data. Citations are to the prettified
  copies in `.research-cache/willow/pretty/` (file:line), to native-module
  disassembly addresses, or to the REA Evidence record.
- **Inferred**: a reasoned conclusion that the code strongly implies but does
  not state outright.
- **Unknown**: not decidable from static analysis.

REA: `analyze_javascript_application` on `app.asar` (integrity policy
record-and-continue, because the 6 unpacked `.node` files were re-signed after
packing) gave parent Evidence `ev_4c20815660fac01072e18fa618dfd6b883d91b8108663966abe2ea2d321f6153`
(asar sha256 `1e1f91b8…0d29a`). That run found 6 BrowserWindows, 3
contextBridge APIs (64 members) and dynamic IPC channel names. The detailed
findings below came from reading the extracted code, plus disassembling the
native modules with capstone (installed in a scratch venv).

## Architecture in one paragraph

Willow is Electron + React (Tailwind v4), with six Windows native addons in
`app.asar.unpacked/out/native/build/Release/`:
- `keyboardmonitor.node`: the global hotkey.
- `audiocapture.node`: WASAPI mic capture.
- `accessibility.node`: UI Automation, clipboard and paste.
- `appinfo.node`: which app has focus.
- `taskbarinfo.node`: where the taskbar is, so the pill sits above it.
- `screenshotcapture.node`: grabs the screen for the "Scribe" assistant.

On a normal dictation, the main process streams 16 kHz PCM over a msgpack
WebSocket to Willow's cloud "middleware". The cloud does the speech-to-text and
the LLM clean-up, and sends back the final text plus a list of "result
actions" (paste, key presses). Willow then pastes the text through a clipboard
"read receipt" system. A local whisper.cpp server is only the offline fallback.

---

## 1. Hotkey

- **Native module**: Willow's own `keyboardmonitor.node`. It is not an npm
  library. **Observed**: its imports include `SetWindowsHookExA` (idType 13
  `WH_KEYBOARD_LL` at `0x18000552d`, idType 14 `WH_MOUSE_LL` at `0x18000559f`),
  `CallNextHookEx`, `GetAsyncKeyState`, `SendInput` and `SetWinEventHook`, and
  it registers Raw Input for mouse buttons 6 and up. The JS wrapper class is at
  `runtime-startup:10713-10834`. Its API: `setKeyBindings`,
  `setPressedKeysCallback`, `startMonitoring` / `stopMonitoring`,
  `setStuckKeyCallback`.
- **Design**: the native hook does not decide anything about modes. It reports
  the *full set of currently pressed keys* (with a timestamp) to JS on every
  change. A JS state machine (`Ft`, `runtime-startup:7396-7900`) does all the
  timing. **Observed.**
- **Default Windows bindings** (**observed**, `runtime-startup:10690-10709`,
  and in the owner's `electron-app-data.json` → `keyboardMonitor.bindings`):
  - Dictation = Left Ctrl (VK 162) + Left Win (VK 91).
  - Scribe/command = Left Win (91) + Left Shift (160).
  - Every key carries `{value, isModifier, shouldBlock, displayName}`.
- **Matching rule** (**observed**, `findMatchedBinding`,
  `runtime-startup:7584-7600`):
  - A binding matches when all of its keys are down and no *extra modifier* is
    down (unless the binding sets `allowExtraModifiers`). Extra non-modifier
    keys are allowed. If several bindings match, the one with the most keys
    wins.
  - A "pure release" event (the pressed set shrinks into a binding) never
    *starts* a binding. Releasing Shift from Ctrl+Shift+Win therefore does not
    fire Ctrl+Win (`runtime-startup:7537-7545`).
  - Escape cancels an active recording.
- **Timing thresholds** (**observed**, `runtime-startup:4593-4597`):
  `longPressDurationSeconds 0.4`, `doubleTapDurationSeconds 0.6`,
  `shortPressTimeoutSeconds 0.65`.
- **Hold-to-talk / double-tap logic** (**observed**, `handleDictationDown/Up`,
  `runtime-startup:7650-7700`):
  1. Key-down starts recording immediately (state `DictationHeld`, emits
     `RecordingStarted`). Audio is captured from the very first millisecond.
  2. Key-up after more than 400 ms ends the recording, and transcription runs.
  3. Key-up within 400 ms moves to `DictationAwaitingDoubleTap` and arms a
     650 ms timer.
     - If the key goes down again within 600 ms of the release, the state
       becomes `HandsfreeRecording` (latched; no hold needed).
     - If the timer fires first, the recording is **silently cancelled**. A
       quick tap does nothing.
     - The timer guards against a busy event loop. If it fired more than
       100 ms late, it re-checks after `setImmediate`, so queued key events
       win (`runtime-startup:7476-7495`).
  4. In hands-free mode, pressing the hotkey again stops and transcribes. A
     press within 600 ms of hands-free *starting* cancels instead.
- **Start menu / Win key**: **observed** facts first.
  - The hook only **blocks (returns 1 for) non-modifier keys** that belong to a
    fully matched binding. In the block decision at `0x180004260`, VK
    0x10-0x12, 0xA0-0xA5 and 0x5B/0x5C are excluded from blocking. **Ctrl and
    Win are always passed through to Windows.** The `shouldBlock: true` flag on
    modifiers is therefore effectively ignored.
  - The only `SendInput` in `keyboardmonitor.node` (`0x1800047bc`) injects a
    **Left-Ctrl key-up** when AltGr is detected (LCtrl immediately followed by
    RAlt 0xA5). That releases AltGr's phantom Ctrl so it cannot trigger
    Ctrl+Win. It is tagged `dwExtraInfo = 0x57494C4C` ("WILL").
  - The masking key lives in `accessibility.node`, in the routine that runs
    before every synthetic shortcut (paste, enter…; `0x1800061f0`, called from
    `0x1800065b0`):
    1. It polls `GetAsyncKeyState` for LWin, RWin, LAlt, RAlt, LShift, RShift,
       LCtrl and RCtrl.
    2. If any **Win or Alt** key is still down, it injects **VK 0xE8**
       (unassigned) down+up as a "menu mask".
    3. It then injects key-ups for every held modifier (with the extended flag
       for RCtrl, RAlt and Win).
    4. Only after that does it send the chord, e.g. Ctrl+V.
    All injected events carry the "WILL" tag.
  - Nothing injects a mask at the moment the user releases Ctrl+Win to finish
    a normal hold. **Inferred**: Willow relies on Windows not opening Start
    because Ctrl was down during the Win press (Windows treats it as a chord).
    The 0xE8 mask covers the dangerous case, where Win is still physically held
    when Willow pastes. **Unknown**: whether Start ever flashes when the user
    lets go of Ctrl before Win.
- **Not breaking other Win shortcuts** (**observed/inferred**):
  - Modifiers are never swallowed, so Win+E, Win+Ctrl+Left and the rest keep
    working.
  - The no-extra-modifier rule means Win+Shift+S never matches Ctrl+Win.
  - A Ctrl+Win+X chord would start a recording, but a quick press gets
    silently cancelled by the 650 ms tap rule.
  - The hook ignores its own injected events: it checks `LLKHF_INJECTED`
    (flags & 0x10) and `dwExtraInfo == "WILL"` at `0x1800045bb-0x1800045cd`.
- **Stuck-key pruning** (**observed**, `0x180003950` and `index.js:1126-1157`):
  - On each event, any modifier in the pressed set that `GetAsyncKeyState`
    says is up gets pruned.
  - JS is told the likely cause: `desktop-switch`, `elevated-window` (UIPI
    swallowed the key-up), `session-transition`, `injected-release`,
    `hook-latency` (event lag of 100 ms or more), or `dropped-by-hook`. The
    native side tracks `msSinceDesktopSwitch` / `msSinceElevatedForeground`
    through `SetWinEventHook`.

## 2. Audio

- **Capture**: native **WASAPI** in `audiocapture.node`. The renderer's
  getUserMedia is not used. **Observed** (strings: "Reusing initialized WASAPI
  audio client", "Retained WASAPI client unavailable, reopening"; JS wrapper at
  `runtime-startup:11315-11480`).
  - The WASAPI client is kept initialised between recordings, for a fast start.
  - If no audio arrives within 5 s (`_w = 5e3`), Willow raises "The microphone
    did not provide audio within 5 seconds".
- **Two streams from one capture** (**observed**, `runtime-startup:19796-19808`):
  - `onLpcm`: Int16, **16 kHz, mono**. The native code resamples with a Media
    Foundation resampler ("Could not resample microphone audio to 16 kHz").
    Each chunk goes to the level meter, to the cloud stream
    (`middlewareController.queueAudio`) and to an optional `.lpcm` file.
  - `onAac`: AAC-LC encoded natively with Media Foundation, written to
    `Recordings/<id>.aac`. The owner's files are **48 kHz mono, about
    64-68 kbps, ADTS** (**observed** with ffprobe), so the AAC branch keeps the
    device rate.
- **Chunking**: the analysis frame is 512 samples / 16 kHz = **32 ms**
  (`Sm = 512`, `runtime-startup:4752-4754`). **Inferred**: native chunks are
  about this size.
- **System audio ducking** (**observed**, `runtime-startup:18533-18575`): when
  `autoDuckAudio` is on, Willow **mutes the Windows system volume** 350 ms
  after recording starts, so the start chime can play first, and unmutes after.
- **Silero VAD + `willow-audio-quality-vad.exe`** (**observed**,
  `runtime-startup:4980-5110`, `5176-5215`):
  - The model is `ggml-silero-v5.1.2.bin`, downloaded once from the public
    ggml-org Hugging Face repo into `VADModel/`. The download is
    SHA-256-checked, capped at 2 MiB and has a 10 s timeout. It is present on
    this PC.
  - **The VAD is not used for live endpointing or trimming.** It runs only
    when a transcription comes back **blank**:
    1. Willow pipes up to 120 s of raw Int16 PCM into the exe on stdin (argv =
       model path).
    2. The exe prints a speech fraction from 0 to 1 (15 s timeout).
    3. Willow classifies the failure as `silence` (VAD speech under 5%, or,
       without the VAD, active speech under 0.3 s or below -55 dB), `clipping`
       (more than 5% of samples in clipped runs of 3 or more at |x| ≥ 32275),
       `narrowband_input` (spectral roll-off ≤ 3800 Hz or input ≤ 8 kHz,
       i.e. a Bluetooth headset in call mode), `quiet_speech` (active level
       below -45 dB) or `speech_but_no_words`.
    4. It shows a toast with playback of the failed clip.
  - The "active speech level" is an ITU-P.56-style iterative estimate: start
    the threshold at -60 dB, put it 15.9 dB under the mean level, repeat up to
    6 times, stop when it moves less than 0.5 dB (`xm`,
    `runtime-startup:4775-4795`).
- **Recording limits** (**observed**, `scheduleRecordingTimeoutTimers`):
  hard stop at 5 min (free) or 8 min (paid), with a warning 30 s before.

## 3. Speech-to-text

- **Cloud is the default.** The "middleware pipeline" is a WebSocket carrying
  msgpack envelopes `{seq, d:{key, data}}`, with oversized messages split into
  `{id, seq, idx, tot, bin}` (`runtime-startup:11915-11945`).
  - **Observed** message keys (`runtime-startup:11659-11700`): `audio_packet`,
    `flush_packet`, `glossary` (the personal dictionary as
    `{term, replacementTerm}`), `context_glossary` (proper nouns scraped from
    the screen), `cursor_before`/`cursor_after` (text around the caret, from
    UI Automation), `app_context`, `selected_text`, `selected_languages`,
    `user_preferences`, `llm_type`, `press_enter_enabled`,
    `smart_insertion_enabled`, `conversation_history`, plus
    `dictation_result` / `result_actions` coming back.
  - The clipboard text (truncated) and the focused app/window are also sent as
    context (`Qw`, `runtime-startup:11798`).
- **Local whisper**: `whisper-server.exe` is whisper.cpp's HTTP server.
  **Observed** (`runtime-startup:6505-6745`):
  - Spawned as `-m <model> --host 127.0.0.1 --port 8089 -t <threads> -nt -sns`.
    That means no timestamps (`-nt`) and suppressed non-speech tokens (`-sns`).
    Threads = 75% of cores, clamped to 2-12.
  - Ready when stdout/stderr says "listening", or `GET /` succeeds (polled
    every 500 ms after 1 s). Startup times out after 30 s. A health check runs
    every 5 s.
  - Requests are `POST /inference` with multipart `file` (WAV),
    `language`, `response_format=json`, and a 120 s timeout. **No prompt, no
    hotwords, no temperature.**
- **When local is used** (**observed**, `Ad()`, `runtime-startup:7106-7112`):
  - Always, when "always use offline mode" is on.
  - Otherwise only when offline mode is enabled *and* the machine is offline.
  - It is also used to re-transcribe interrupted recordings when the cloud
    fails.
- **Which model**: **unknown**. The download URL comes from remote config
  (`OFFLINE_MODEL_ELECTRON_WINDOWS_URL`). The finder accepts any `.bin`,
  `.gguf` or `*turbo*` file in `OfflineModel/`. That folder does not exist on
  this PC, so no model was ever downloaded.
- **Dictionary as prompt**: in the cloud, yes (the `glossary` message). Locally
  it is not passed to whisper. **Observed.** Replacements are applied
  afterwards in JS (see §6).
- **Language**: the owner's `settings.preferredLanguage` = `en-gb`.

## 4. Paste

**Observed**, `runtime-startup:14560-14900`, plus `accessibility.node`
disassembly and strings.

1. **Serialize**: every insertion runs inside
   `accessibility.runExclusivePaste`, so two pastes can never interleave.
2. **Focus check and restore**: when the overlay appears, Willow calls
   `captureForegroundWindowTarget()`, which records the foreground HWND. Before
   pasting it can call `restoreTextInsertionTarget()`. The native code uses
   `AttachThreadInput`, `SetForegroundWindow`, `BringWindowToTop`,
   `SetActiveWindow` and `IsIconic`.
   - `hasFocusedTextField` uses UI Automation and returns a reason:
     `own-foreground`, `desktop-or-shell`, `text-control-type`,
     `non-text-widget`, `readonly-browser-page` or `default-proceed`.
3. **Clipboard read receipt (Windows)**: this is the clever part.
   1. `writeClipboardReadReceipt(text)` puts the text on the clipboard from a
      hidden window class `WillowClipboardReadReceiptOwner` (owner window +
      `RegisterClipboardFormatW`). **Inferred**: it uses delayed rendering, so
      the owner gets `WM_RENDERFORMAT` at the moment the target app actually
      reads the clipboard.
   2. It also sets the formats `ExcludeClipboardContentFromMonitorProcessing`,
      `CanIncludeInClipboardHistory` and `CanUploadToCloudClipboard`, so
      transcripts stay out of Win+V history and cloud clipboard.
   3. If the write fails, retry once after 100 ms.
   4. Send **Ctrl+V** (`simulateKeyPress("paste")`). This goes through the
      modifier-release and 0xE8 mask routine from §1.
   5. Poll `wasClipboardReadReceiptRead()` **every 10 ms for up to 1000 ms**.
   6. If it was read: call `finishClipboardReadReceipt()` **500 ms later**,
      which restores the previous clipboard (log string "Clipboard restored").
   7. If it was not read within 1 s: retry with **Ctrl+Shift+V**
      (`paste-fallback`, for terminals) using the same receipt flow.
   8. If that still was not read: leave the transcript on the clipboard and
      show a toast: "Your transcript is copied. Paste it into a text field."
4. **Non-receipt path** (macOS and older): try `insertTextIntoFocusedField`
   (an accessibility value set) first. Then fall back to `fastPaste`
   (clipboard write + Ctrl+V), wait **100 ms** (`fastPasteSessionEndDelayMs`)
   and end the session. If that fails, copy to the clipboard and toast.
5. **Action sequences** from the cloud (`result_actions`): `paste` (then a
   60 ms pause), `keyPress` (enter / tab / escape / space / delete) and
   `delay` (n ms). This is how "press Enter after dictating" works.
6. **No character-by-character typing fallback.** **Observed**: there is no
   unicode `SendInput` path.
7. **Inferred**: clipboard backup covers text only. `accessibility.node`
   imports no `EnumClipboardFormats`, so images or rich formats on the clipboard
   would be lost.

## 5. Auto-learning dictionary (top priority)

**Observed**, `runtime-startup:13994-14560` (TextMonitor `VC`,
AutoDictionaryManager `eE`), the diff helpers at `runtime-startup:1915-2030`,
and the trusted-app lists at `index.js:3100-3205`.

**Detection uses UI Automation polling. Willow does not watch keystrokes or
the clipboard.**
1. After a successful paste (`Vr(text, tracking)`), `autoDictionaryManager.startTracking(pastedText, {transcriptId, targetAppInfo, dictationEventId})` runs:
   - It skips text under 3 characters.
   - It re-reads the focused app (500 ms timeout) and aborts if the bundleId,
     window title or URL differs from the dictation's target. The user moved
     on.
   - It skips blacklisted apps: terminals, IDEs (VS Code, Cursor,
     JetBrains…), Git clients, Postman and similar (`Fa`, `index.js:3108`).
   - It marks the target **trusted** only for email and chat apps: Apple Mail,
     Gmail, Google Docs, Outlook, Slack and Superhuman (by bundle id, app name
     or host: `Va`, `index.js:3190`).
2. **Polling**: `accessibility.getCursorContextAsync()` returns
   `{textBefore, selectedText, textAfter}` for the focused element through the
   UIA TextPattern ("Text pattern not supported by focused element" is the
   failure string). The full field text is `before + selected + after`.
   - The first poll is at 250 ms. After that every **500 ms** while under
     **10 s** since the start or the last change, then every **2000 ms**.
   - Each UIA call has a 1500 ms timeout. 3 consecutive failures stop tracking
     ("focus lost").
   - Hard cap: **300 s (5 min)**.
   - Placeholder texts ("Say hello", "Add your comment here…", "Tip: Hit ⌘…")
     count as an empty field.
3. **Guards on each poll** (`OC`):
   - **First poll**: the whole field must **equal the pasted text exactly**,
     after CRLF→LF and NFC normalisation. Otherwise tracking stops with
     `span-mismatch`. Willow therefore only learns when the dictation *is* the
     field content (a fresh message or compose box).
   - Field empty → stop (sent or cleared). The last known text is kept.
   - Field longer than max(10 × the pasted length, 5000 chars) → stop.
   - New text not "similar" to the last known text (see below) → stop. A big
     jump means the user switched fields or pasted something else.
   - **Enter** without Shift ends tracking (the message was sent). This rule
     is skipped for email apps, where Enter is a newline.
4. **When tracking ends**, `Tn(initial, final)` must pass: both 3-4000
   characters, not identical, and similar. The similarity test `Il`, applied to
   lower-cased trimmed text:
   - length ratio ≥ 0.25;
   - if both are ≤ 64 characters: normalised Levenshtein similarity ≥ 0.3;
   - otherwise word-bigram Dice ≥ 0.4 (when each has at least 2 words), else
     character-4-gram Dice ≥ 0.4.
5. **What happens with a valid edit**:
   - It is always saved on the transcript as `beforeEdit` / `afterEdit`
     (`recordPostPasteEdit`). The last 10 unique edits go to the LLM as "edit
     history" so it learns the user's style (`getRecentEdits`,
     `runtime-startup:2475`, `12907`).
   - It is sent to the cloud diff endpoint (unless privacy mode is on).
   - **Only for trusted targets**, it goes to cloud
     `AUTO_DICTIONARY.EXTRACT_PROPER_NOUNS` with
     `{original_transcription, corrected_transcription}`. The server returns
     `proper_nouns[]`. **The decision about *what* to learn is server-side and
     not visible.**
   - The terms are de-duplicated case-insensitively and shown in a pill toast:
     `"X" was added to dictionary`, "Willow will remember this spelling from
     now", with an **Undo** button. They sit in `pendingCommits` and are
     committed when the toast resolves without Undo (IPC
     `CommitAutoDictionaryTerms` vs `UndoAutoDictionaryTerms`). Each one is
     added as a plain term (no replacement), and existing terms are skipped.
6. **Storage**: `dictionary.json` (see §9), category `user`. Team terms come
   from the server and are merged at paste time.
7. **A local proper-noun extractor does exist**
   (`proper-noun-extractor-*.js`, built on `compromise` NLP). It takes people,
   places and organisations, 2-60 characters, initial capital, each word at
   least 2 characters, minus a stop list of greetings, months, weekdays and UI
   verbs. **But it is used for screen-context glossary**, not for corrections:
   it scrapes the foreground app's UIA tree (Slack, VS Code, Outlook and Teams
   have custom explorers) to bias recognition.
8. **"Replace X with Y" rules**: stored as dictionary entries with
   `replacement` and `isReplacementEnabled: true`. Willow calls these
   "shortcuts"; the owner's are e.g. "my email shortcut" → his address. See §6
   for how they are applied.

## 6. Tidy-up

**The cloud does the real clean-up**: fillers, punctuation, formatting and tone.
The **prompt is not in the client** (**unknown**; no "You are…" prompt text
exists in any bundle). What the client sends to steer it (**observed**):
- `llm_type` (`normal` / `email`; the owner's transcripts show 1059 normal and
  4 email).
- `style_matching` per category (`casual_messages`, `work_messages`, `email`,
  `other`), each with `writing_length` ∈ {direct, natural, thorough} and free
  text `personal_tweaks`.
- The user's "knowledge" entries, the personal dictionary and recent edit
  pairs.

**Local post-processing** (**observed**, `Wc.processText`,
`runtime-startup:18330-18412`). Wc.processText runs on the result before
pasting, in this order:
1. `cleanText`: strip `*markdown emphasis*` and `<output>` tags; turn `—` and
   `--` into ", ".
2. **Dictionary replacements**:
   - If the *whole* utterance equals an enabled shortcut's trigger (case
     insensitive, trailing `.!?,;:` allowed), the output becomes the
     replacement text alone.
   - Otherwise every term with a replacement is applied, longest first, as
     whole-word, case-insensitive matches (`\bterm\b\.?`).
3. **Smart insertion** (needs `textBefore`/`textAfter` from UIA within
   1500 ms):
   - Lower-case the first letter if the preceding text doesn't end a sentence
     and ends in a letter or digit. "I", "I'm" and acronyms are kept.
   - Drop the final punctuation if the text after the caret starts with
     punctuation or a lower-case word.
   - Add a leading space unless the text before ends in whitespace, an opening
     bracket/quote or a newline; add a trailing space unless the text after
     starts with whitespace or closing punctuation.
   - With no context: `trim() + " "`.
4. **Style** by app category: Formal = unchanged.
   - **Casual**: drop the final `.`/`!` (keep `?`).
   - **Extremely casual**: lower-case everything; drop commas, semicolons and
     final punctuation; slang swaps (let me know→lmk, by the way→btw,
     you→u…).
   - **Excited**: turn up to 2/3/4 sentences (for text under 50, under 150, or
     longer) containing thanks / amazing / definitely-type words into "!".
5. French: a space before `? ! ; :` when French is the only language.
6. Referral email signature (marketing; ignore).

**There is no local filler-word list.** Searches for "um"/"uh" found nothing.

## 7. Popup / overlay ("bar")

**Window** (**observed**, `index.js:3406-3416`, `3778-3815`, `3540-3605`):
- **525 × 500 px**, transparent (`#00000000`), frameless, `alwaysOnTop`,
  `skipTaskbar`, `focusable: false`, `resizable: false`, `hasShadow: false`,
  `type: "toolbar"` on Windows.
- `setIgnoreMouseEvents(true)` (click-through) except while interactive.
- Shown with `showInactive()`, so it never steals focus.
- Position: bottom-centre of the work area (from `taskbarinfo.node`, which
  handles an auto-hidden taskbar), **6 px** above it. Docking left, right and
  bottom-left/right is supported (8 px edge inset).
- A 1000 ms tick follows the **display under the cursor**, re-asserts
  `setAlwaysOnTop(true, "screen-saver")` every 5 ticks (and right after
  show), and re-checks the taskbar every 5 ticks.
- The visible pill is only a small part of this big transparent window. Toasts
  and the Scribe panel grow inside it.

**States and sizes** (**observed**, `BarWindow-*.css:30-130`):

| State | Size | Look |
|---|---|---|
| Idle ("rest pill", shown when `showBubbleWhenNotRecording`) | **32 × 7 px**, radius 16 | `#1e1e20`, 0.75 px `rgba(255,255,255,.2)` border, 2 px off the bottom |
| Recording / processing | **100 × 36 px**, radius 18 | same surface, 1 px border |
| Hands-free | **135 × 36 px** | wider; *inferred* to make room for the stop/cancel controls |
| Degraded (free-tier limit) | | 0.25 px border `#ee7811` (orange), small "mini mode" badge |
| Scribe | | rotating conic-gradient border `#e94cb1 → #f5dcb0 → #76d7f0 → #b790e0`, 4 s linear loop, soft pink/cyan glow |
| Toast / notice / error | grows | bar-toast with icon, title, description, action button and a countdown ring (`stroke-dashoffset` animation) |

Pipeline status enum: idle, initializing, recording, stoppingRecording,
transcribing, processing, complete, error. While transcribing, the waveform
cross-fades (0.15 s ease-out) to a white spinner rotating once every 2 s
(linear).

**Animation**:
- The surface animates width, height, radius, border colour and margins over
  **0.3 s `cubic-bezier(0.2, 0, 0, 1)`**, from `transform-origin: bottom
  center`.
- The inner pill content fades in **0.15 s ease-out after a 0.2 s delay**, so
  it appears once the shape has mostly grown, and hides instantly.
- The Scribe layer fades over 0.2 s (0.1 s delay); toasts over 0.18 s (0.08 s
  delay).
- Mode icons pop in with scale 0.5→1 and fade, 0.2 s ease-out.

**Waveform / level meter** (**observed**, `BarWindow-*.js:113-135`,
`1385-1505`):
- Main process: RMS dB of each 32 ms chunk, `20·log10(rms)`, sent to the
  overlay at most every **50 ms**.
- Renderer: level = clamp((dB + 60) / (−34 + 60)). That maps −60 dB to 0 and
  −34 dB to 1.
- **11 bars**, each 2 px wide × 18 px tall, gap 2 px, white, radius 1.125 px.
- Each animation frame:
  1. Push `level^1.15` into a 10-slot ring buffer.
  2. Bar *i* reads the sample delayed by `round(d·9)` frames, where *d* is its
     normalised distance from the centre (0 at the centre, 1 at the edges),
     and scales it by `1 − 0.55·d`. The centre leads and the edges echo it.
     The result is a ripple outwards.
  3. A tiny idle shimmer `0.06·(0.5 + 0.5·sin(1.6·t − 0.9·π·d))` sets a floor
     (skipped when reduced motion is on).
  4. Smoothing: `v = v·k + target·(1−k)`, with `k = 0.87^(dt·60)` and dt capped
     at 0.1 s.
  5. Output `scaleY = 0.14 + 0.86·v`, `opacity = 0.5 + 0.45·v`.

**Sounds** (**observed**, `BarWindow-*.js:71-95`):
- `start-3.mp3` (0.52 s), `end-3.mp3` (0.52 s), `premium-reached`, `happy`,
  and a `chime` for word-count milestones.
- All play through `new Audio()` at volume 1.

## 8. Main window UI (light theme)

**Window**: **1250 × 900** default and minimum, hidden title bar, closing it
hides to the tray (**observed**, `index.js:3661-3662`, `3712-3745`).

**Layout** (**observed**, `MainLayout-*.js`):
- Window background `--shape2 #f3f3f5`.
- A **190 px sidebar** (`p-2`) on the left: logo wordmark (display font,
  medium, 20 px), then nav items, then promo cards at the bottom.
- The content sits in one **white panel** (`--panel #fff`) inset 6 px
  (`m-1.5 ml-0`), with **radius 28 px** and a 1 px `rgba(0,0,0,.06)` border.
- **Nav item**: full width, gap 8 px, padding 10 × 6 px, **radius 12 px**,
  16 px text, `text1`. Active = `#2121210D` fill; hover = `#21212108`;
  transition 150 ms ease-out; 20 px icon column.
- **Pages**: Home, Knowledge, Dictionary, Shortcuts, Collaboration, Team
  Members (plus a Settings modal, Learning Center, Referrals, and Scribe
  history).
- **Home**: page padding `px-10 pt-8 pb-4` (40 px sides).
  1. A **4-column stats row** (gap 32 px): Dictated words · Time saved · Day
     streak · Average speed. Each tile has a 14 px `text2` label (6 px below
     it), a **20 px** `text1` value (line-height 1) and a 12 px unit at 60%
     opacity.
  2. Below it, the **History** list: a sticky header (`text-h3` 17 px
     semibold, sort menu), grouped under "Today" / "Yesterday" / dates.
     - Each row has the time in a 72 px column (`text3`, 12 px medium), then
       the transcript (14 px `text1`), with copy / re-transcribe / report /
       delete actions on hover.
     - Row separators are 1 px `--separator2` with 16 px side margins.
  3. A shareable stats card sits on a purple gradient (white text, 27 px bold
     numbers).

**Light tokens** (**observed**, `index-*.css:6048-6132`):

| Token | Value | Token | Value |
|---|---|---|---|
| panel | `#ffffff` | primary | `#4443aa` |
| shape1 / surface | `#fdfbf9` | primary2 | `#373667` |
| shape2 / background | `#f3f3f5` | primary3 | `#5446a4` |
| shape3 | `#ebebf0` | primary-subtle | `#e9e4fb` |
| shape4 | `#f5f7fa` | primary-border | `#d6d3fd` |
| settings-bg | `#fafafa` | fill-1 | `#efeaff99` |
| text1 | `#3d3c40` | fill-border | `#dfd9f9` |
| text2 | `#8a888a` | key-pill-bg | `#b6a0ff1f` |
| text3 | `#b0afaf` | visualizer-bar | `#cfcee9` (active = primary) |
| text-muted | `#21212199` | separator | `#e9e8e8` |
| icon-default | `#3d3d3a` | separator2 | `#4543ab14` |
| input-fill / stepper | `#f2f4fa` | border-card | `#0000000a` |
| card-bg | `#f8f8f8` | window-border | `#4543ab33` |
| button-secondary | `#f7f7f8` | alert | `#e5484d` |
| selected-bg | `#2121210d` | success | `#00ac95` |
| toast | `#0d0d0dcc` | shape-shadow | `#3835660f` |

Shadows:
- `--shadow-elevated`: `0 2px 8px #38356614, 0 1px 2px #3835660f`.
- `--drop-shadow-card`: `drop-shadow(0 4px 23.7px #00000008)`.
- Cards use `0 4px 12px rgba(0,0,0,.03)`.

Purple-tinted, very low-alpha shadows are a big part of the "finish".

**Type**:
- **Inter Variable** (bundled woff2, weights 100-900) for everything, with a
  system fallback. Text is not selectable app-wide.
- Size scale: 40 / 32 / 28 / 24 (h1) / 22 (h2) / 20 / 17 (h3, body) / 16 (h4) /
  15 (h5) / 14 (p1) / 13 (p2) / 12 (p3) / 10.

**Radii**: 12 (nav, menus), 14, 16, 18, 22 (cards), 28 (main panel), full
pills for buttons. Primary buttons are 33 px tall pills, `bg-primary`, 13 px
medium white text, hover at 90% opacity.

**Spacing**: 4 px base (Tailwind). Pages use 40 px horizontal padding.

The dark theme exists (`panel #141414`, `primary #4f3a97`, text `#ededed`) but
the owner prefers light.

## 9. Storage formats (for importing the owner's data)

- **`dictionary.json`** (**observed**; the owner has 4 entries):
  ```json
  { "terms": [ { "id": "uuid", "original": "string", "replacement": "string|null",
                 "isReplacementEnabled": true, "dateAdded": "ISO-8601", "category": "user|team" } ],
    "lastUpdated": "ISO-8601" }
  ```
  Import rule: `isReplacementEnabled && replacement` → a Wren "replace X with
  Y" rule; otherwise a vocabulary word. All 4 of the owner's current entries
  are shortcuts (trigger phrase → expansion).
- **`Transcripts/<uuid>.json`**: one file per dictation; 1080 on this PC.
  - Fields: `id`, `query` (the final text), `timestamp` (ISO),
    `isPlaceholder`, `status` (`completed` 1060 / `failed` 20, with `error`),
    `dictationEventId`, `audioFilePath` (absolute path),
    `appName`, `llmType`, `style`, and
    `middlewareContext: {focusedAppInfo: {success, appName, bundleId, windowTitle}, glossary: [{term}], onboardingStep}`.
  - Optional fields: `beforeEdit` / `afterEdit` (7 files: the learned
    corrections) and `scribeTurns: [{userTranscript, scribeResponse}]`.
  - **There is no raw (pre-LLM) transcript stored locally.** Only the final
    text.
- **`Recordings/<same uuid>.aac`**: AAC-LC, ADTS, **48 kHz mono, about
  64-68 kbps**. It pairs with its transcript by **identical UUID filename**
  (and `audioFilePath`). There are 1136 `.aac` files.
  - `.lpcm` = raw 16 kHz / 16-bit / mono, kept only in offline mode. It gets a
    44-byte WAV header when sent to whisper. The 14 `.lpcm` files here are
    0-byte orphans.
- **`RecordingRecovery/<uuid>.json`**: `{version: 1, transcriptId, kind: "dictation"|"scribe", startedAt}`,
  written at recording start and removed on completion. On launch, Willow
  finds the matching `.aac`/`.wav`/`.lpcm` and offers to re-transcribe.
  - Cleanup: "auto-delete transcripts" (a team setting) deletes transcripts
    and recordings older than 24 h, in batches of 300.
- **`electron-app-data.json`**: a flat key-value store. Values are
  JSON-encoded strings, e.g. `keyboardMonitor.bindings`,
  `settings.preferredLanguage`, `settings.showBubbleWhenNotRecording`,
  `settings.privacyMode.enabled`.
- `Dictionaries/en-US-10-1.bdic` is Chromium's spellcheck dictionary. It has
  nothing to do with the user dictionary.

## 10. Stats

**Per dictation, words per minute** (**observed**, `zl`,
`runtime-startup:4613`):
- WPM = words / recording seconds × 60.
- Skipped when the recording is under 3 s.
- Discarded if ≤ 0 or > 300.

**Lifetime figures**: word count, streaks and the aggregate WPM are kept **on
the server** (`words_spoken`, `current_streak`, `longest_streak`,
`words_per_minute`). Offline word counts queue as a delta and sync later. The
streak rule itself is server-side (**unknown**; inferred to count consecutive
local days with at least one dictation).

**"Time saved"** (**observed**, `u0`, `MainLayout-*.js`):

```
minutes = max(0, round(words / 50 − words / max(wpm, 80) × 1.3))
```

That is:
- typing baseline **50 WPM**;
- speaking speed floored at **80 WPM**;
- ×1.3 overhead for pauses and fixes.

Display: `Xh Ym` or `Ym`.

## 11. Other clever bits worth borrowing

- **Synthetic input hygiene**:
  - Tag every injected event (`dwExtraInfo` magic) so your own hook ignores it.
  - Release any held modifiers before a chord.
  - Mask Win/Alt with VK 0xE8 before injecting.
  - Handle AltGr's phantom Ctrl.
- **Stuck-modifier recovery**: cross-check the hook state against
  `GetAsyncKeyState` on every event. UIPI (elevated windows), desktop switches
  and lock screens drop key-ups.
- **Clipboard read receipt**: proves the paste landed, and only then restores
  the clipboard. That avoids the classic "restored too early, pasted old
  clipboard" race. The receipt also opts the transcript out of clipboard
  history and cloud sync.
- **Ctrl+Shift+V fallback** for terminals that don't take Ctrl+V.
- **Smart insertion** with UIA caret context (spacing and capitalisation at
  the join).
- **Mic selection**:
  - Order: `default` first, then built-in, wired, wireless, virtual.
  - On disconnect mid-recording, switch to the best fallback and tell the user.
    Switch back automatically when the original mic returns.
  - Cache the device list for 5 s.
  - Warn about Bluetooth headsets in call mode (narrowband).
- **Blank-result diagnosis** with VAD plus level/clip/roll-off metrics, and an
  inline replay of the failed clip. This beats a generic "nothing heard" toast.
- **Crash-safe recordings**: a manifest written at start, audio streamed to
  disk, and re-transcription on the next launch.
- **Topmost re-assertion** every 5 s, so full-screen or other topmost windows
  can't bury the pill; `showInactive` and non-focusable so it never steals
  the caret.
- **Muting system audio** while dictating (optional), delayed 350 ms so the
  start chime still plays.
- **Screen-context glossary**: the foreground app's visible text →
  proper-noun extraction (compromise) → recognition bias. This is a good
  offline idea for Wren: feed it into whisper's `prompt`.
- **Tap-timer lag compensation** (re-check after `setImmediate` when a timer
  fires more than 100 ms late).

---

## Design takeaways for Wren

1. **Hotkey**:
   - Use a `WH_KEYBOARD_LL` hook on its own thread that *reports the pressed
     set* and lets JS/TS run the state machine.
   - **Never block modifiers.** Only block non-modifier keys of a fully
     matched binding.
   - Require an exact modifier set, and never start a binding on a release
     event.
   - Thresholds: **hold > 400 ms = push-to-talk; release ≤ 400 ms and second
     press within 600 ms = hands-free latch; otherwise cancel at 650 ms**.
   - Start capturing at key-down.
   - Escape cancels.
2. **Start menu**: copy Willow's mask: if Win or Alt is down, inject VK `0xE8`
   down+up, then release the held modifiers, then inject the chord. Go one
   better and **also send the 0xE8 mask the moment Ctrl+Win activates**, which
   removes Willow's "unknown" gap. Tag all injected input with a magic
   `dwExtraInfo` and ignore it in the hook.
3. **Stuck keys**: on every hook event, prune modifiers that `GetAsyncKeyState`
   reports up. Log the cause (elevated foreground, desktop switch).
4. **Audio**:
   - WASAPI shared mode, kept warm between recordings.
   - Resample to **16 kHz mono Int16** in **32 ms (512-sample) chunks**.
   - Save **16 kHz mono WAV or FLAC** locally. Wren is offline, so no 48 kHz
     AAC is needed.
   - 5 s "no audio" timeout.
5. **VAD**: use Silero v5 (`ggml-silero-v5.1.2.bin`, which already sits in the
   owner's `VADModel/`) for **blank-result diagnosis** with Willow's thresholds:
   speech fraction under 5% = silence; clip runs of 3+ samples at |x| ≥ 32275
   over 5% of the clip = clipping; roll-off ≤ 3800 Hz = Bluetooth call mode;
   active level below -45 dB = quiet. Unlike Willow, also use it to **trim
   leading and trailing silence** before whisper.
6. **STT**: whisper.cpp server on localhost. Willow runs `-nt -sns` with
   threads = clamp(0.75 × cores, 2, 12). **Pass the dictionary as whisper's
   `prompt`**, which Willow does not do locally, plus the screen-context proper
   nouns.
7. **Paste**:
   1. Serialise insertions.
   2. Capture the foreground HWND when recording starts and restore it before
      pasting.
   3. Clipboard write with **delayed rendering** from a hidden owner window, so
      you know the target read it. Exclude it from clipboard history and cloud
      sync.
   4. Send Ctrl+V and poll every **10 ms for up to 1000 ms**.
   5. Restore the clipboard **500 ms after the read**.
   6. Fall back to **Ctrl+Shift+V**, then to "copied, paste it yourself".
   7. Back up *all* clipboard formats (`EnumClipboardFormats`). Willow only
      keeps text.
8. **Auto-learning dictionary**: implement Willow's UIA loop, with the learning
   step done locally.
   1. After a paste, poll the focused element's TextPattern: first at 250 ms,
      then every **500 ms** while under 10 s since the start or the last
      change, then every **2000 ms**, with a **5 min** cap.
   2. Stop on Enter (but not Shift+Enter, and not in email), on an empty field,
      on 3 UIA failures, or when the text stops resembling the dictation
      (Levenshtein ≥ 0.3 for ≤ 64 chars, else bigram Dice ≥ 0.4).
   3. Relax Willow's "field must equal the paste exactly" rule. Locate the
      pasted span inside `textBefore` instead, so dictating into an existing
      document still learns.
   4. Diff the paste against the final text at **word level** and keep
      substitutions of 1-3 words where the new token is capitalised, unusual
      (not in a common-words list) or close in sound or spelling. Willow's
      compromise rules give a stop list and shape filter: 2-60 characters,
      initial capital, every word at least 2 characters.
   5. Show an **Undo toast** and commit when it times out.
   6. Store the pair as `{original: heard, replacement: corrected}` for
      repeated corrections, or as a vocabulary word otherwise. Keep
      `beforeEdit/afterEdit` on the transcript as an audit trail.
   7. Skip terminals and IDEs (reuse Willow's blacklist idea).
9. **Tidy-up (local, no LLM)**:
   1. Strip `*emphasis*`; turn `—` and `--` into ", ".
   2. Apply dictionary replacements: a whole-utterance shortcut match returns
      the expansion alone; otherwise longest-first, whole-word,
      case-insensitive.
   3. Apply smart insertion rules from caret context, with a 1500 ms UIA
      budget.
   4. Add a filler list (um, uh, er, "you know" at clause start), which Willow
      leaves to the cloud. Optionally a casual/formal style per app.
10. **Overlay**:
    - A 525 × 500 transparent, click-through, non-focusable `toolbar` window,
      bottom-centre, 6 px above the taskbar, following the cursor's monitor
      every 1 s. Re-assert `screen-saver` topmost every 5 s. Show it with
      `showInactive`.
    - Pill: idle **32 × 7** (radius 16) → recording **100 × 36** (radius 18) →
      hands-free **135 × 36**.
    - Surface `#1e1e20` with a `rgba(255,255,255,.2)` border; shape animates
      **0.3 s cubic-bezier(0.2, 0, 0, 1)**; content fades in after a 0.2 s
      delay.
    - **11-bar ripple waveform** with the exact maths in §7 (−60 → −34 dB
      window, ^1.15, centre-out delay of 9 frames, 0.55 edge falloff,
      0.87^(60·dt) smoothing, 0.14 minimum scale), with levels throttled to
      50 ms.
    - A spinner (2 s per turn) while transcribing.
11. **Main window**:
    - 1250 × 900; `#f3f3f5` canvas; 190 px sidebar; white content panel with a
      28 px radius, 6 px inset and a 6%-black hairline border.
    - Inter Variable throughout. Primary `#4443aa`, text `#3d3c40` / `#8a888a`
      / `#b0afaf`, active nav `#2121210D` at radius 12, purple-tinted
      low-alpha shadows (table in §8).
    - Home = 4 stat tiles (20 px values) above a day-grouped history list.
12. **Stats**:
    - Per-dictation WPM = words ÷ minutes, ignoring recordings under 3 s and
      values over 300.
    - Time saved = `max(0, round(w/50 − w/max(wpm, 80)·1.3))` minutes.
    - Streak = consecutive local calendar days with at least one completed
      dictation (Wren's own definition, since Willow's is server-side).
13. **Import**:
    - Read `dictionary.json` as described in §9.
    - Optionally import `Transcripts/*.json` + `Recordings/<id>.aac` into Wren
      history, joined on the UUID, with `query` as the text and `timestamp`.
    - The 7 `beforeEdit/afterEdit` pairs are free seed data for the learner.

---

## Checklist

| # | Question | Status |
|---|---|---|
| 1 | Hotkey: library, hold/double-tap timings, Start-menu masking, other Win shortcuts | **Answered.** One partial point: masking at hotkey *release* is not done by Willow (observed); whether Start can still flash is unknown. |
| 2 | Audio capture, format, chunking, VAD exe + Silero | **Answered** (chunk size inferred from the 512-sample analysis frame) |
| 3 | STT local vs cloud, whisper params, dictionary prompt, post-processing | **Partial.** Routing, args and params answered; the offline model file is unknown (remote-config URL, never downloaded here). |
| 4 | Paste mechanism, delays, fallbacks | **Answered** (the delayed-rendering detail and the text-only backup are inferred) |
| 5 | Auto-learning dictionary detection, window, diff, storage; replace rules | **Answered** for detection, windows, guards and storage. **Partial**: the final "which words to learn" step is a cloud endpoint and not visible. |
| 6 | Non-LLM tidy-up rules; LLM prompt | **Answered** for local rules (no filler removal locally). **Unresolved**: the LLM prompt is server-side. |
| 7 | Overlay window options, states, animation, waveform | **Answered** (exact hands-free control layout not captured) |
| 8 | Main window theme tokens, pages, home layout | **Answered** (light tokens, type, radii, shadows, layout; individual icons not catalogued) |
| 9 | dictionary.json, transcripts, recordings | **Answered** |
| 10 | Time saved, WPM, streaks | **Partial.** Time-saved and per-dictation WPM answered; streak and aggregate WPM are server-side. |
| 11 | Other clever bits | **Answered** |
