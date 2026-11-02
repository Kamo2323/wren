# Wren: Goldfish brainstorm report

- **Date:** 2026-11-02
- **Mode:** BRAINSTORM (a multi-lens attack on the concept before any design doc exists)
- **Input:** `C:\Wren\docs\design\concept-input.md`
- **Purpose:** this feeds an unattended overnight build. The owner can't be asked anything tonight, so every open question below comes with a default.

## Reviewer status

| Reviewer | Lens it ran as | Status |
| --- | --- | --- |
| `egm-design` | Skeptical architect and resource economics | completed |
| `egm-codebase-details` | Reuse and grounding (there's no code yet, so: which libraries already solve this) | completed |
| `egm-zoom-out` | Contrarian / YAGNI (cut anything not needed) | completed |
| `egm-comprehension` | UX and ergonomics, plus a naive reader looking for ambiguity | completed |
| `egm-implementation-readiness` | Can unattended builders start tonight and prove it works? | completed |
| `egm-structural` | Maintainability, module boundaries, and the concept doc as a spec | completed |

**How to read this**
- Severity: 🔴 **Blocker** (could stop a working app by morning), 🟡 **Major** (fix in v1, but the app still works without it), 🟢 **Polish**.
- No reviewer had web access. Library names, API names and timing or memory figures come from their own knowledge. Anything not firmly known is marked *(belief)*. The items to check in the first hour are collected in [§7](#7-beliefs-to-verify-in-the-first-hour).
- Where reviewers disagreed, the report says so and picks a default (marked **Synthesis call**).

## The short version

- **The hotkey and the paste are where this app lives or dies.** All six reviewers independently put the same three things at the top:
  - Ctrl+Win is the start of many real Windows shortcuts.
  - The popup must never take keyboard focus.
  - Ctrl+V must not be sent while Win is still held down, or Windows treats it as Win+Ctrl+V.
- **Don't let the model benchmark hold up the build.** Willow's transcripts are not a valid answer key: they contain the very mis-hearings the owner complains about. Ship faster-whisper `large-v3-turbo` tonight and make the benchmark a morning report.
- **Correction learning in the Claude desktop app is the riskiest feature.**
  - Build learning from the History screen first.
  - Tonight, run only a probe of what UI Automation (Windows' accessibility interface) can read from Claude's text box.
  - Learned words should nudge the model ("terms") long before they ever turn into hard find-and-replace rules.
- **Overnight verification is a blocker of its own.** Live tests must never type into the owner's real Claude chats. They also stop working if the PC locks. Use a test-only Electron window and recorded WAV files instead.
- **A strong reuse candidate exists:** Handy (github.com/cjpais/Handy, MIT licence) is said to use almost exactly the proposed stack. *(belief: its current features haven't been checked)*

---

## 1. Top risks that could block a working app by morning (ranked)

### R1 🔴 The keyboard hook breaks Windows shortcuts, opens Start, or dies silently
*Raised by: all six lenses. Confidence: high.*

- **What goes wrong**
  - Ctrl+Win is the opening of real shortcuts:
    - Ctrl+Win+Left/Right/D/F4 (virtual desktops)
    - Ctrl+Win+Enter (Narrator)
    - Ctrl+Win+O (on-screen keyboard)
    - Ctrl+Win+C (colour filters)
    - Ctrl+Win+Shift+B (graphics driver reset)
    - Win+Ctrl+V (Windows 11 sound-output panel) *(belief, medium confidence)*
  - The concept doesn't say what happens when any of these is pressed.
  - A low-level keyboard hook is a Windows callback that sees every key press on the PC. If it takes too long to return (the `LowLevelHooksTimeout`), Windows removes it without telling anyone, and the hotkey just stops working. Until that happens, every keystroke on the PC lags.
  - The lock screen or a UAC prompt can swallow a key-up, which leaves Wren stuck "recording".
- **Rules that fix it**
  1. **Never block Ctrl or Win.** Every key event is passed on (`CallNextHookEx`). Wren only watches.
  2. **The chord** is Left Ctrl (VK 0xA2) plus Left Win (0x5B), pressed in either order, with no other key down. Right-hand keys and auto-repeat key-downs are ignored.
  3. **Start recording on chord-down,** so the first word is never lost. If any other key goes down during the chord (Shift and Alt included), cancel quietly, throw the audio away, and let the shortcut work as normal.
  4. **Start-menu mask.** Inject a harmless unused key (VK 0xE8, the key AutoHotkey uses for this) so Windows doesn't treat the Win release as "open Start".
     - **Synthesis call:** two reviewers point out that a key injected at the moment of Win release may be queued *behind* the real Win-up, so Start can still open *(belief)*.
     - So inject the mask **as soon as the chord is recognised, while Win is still held**, and do it for every tap of a double-tap.
     - Test both timings in the first hour, and keep the one that suppresses Start in 50 out of 50 tries.
  5. **Tag Wren's own injected keys** with a marker in `dwExtraInfo` and ignore only those. Otherwise Wren's own mask key and Ctrl+V feed back into its state machine. Other injected keys are still accepted, so automated tests can drive the hotkey.
  6. **The callback does almost nothing.** It classifies the key, updates a tiny state machine, and posts a message to another thread. No logging, audio, IPC, clipboard, database or UI Automation inside it. The hook lives in native Rust on its own thread with a message loop; a Python hook would stall while Python is busy transcribing.
  7. **Watchdog.** Re-install the hook after sleep or resume, and when the PC has recent input (`GetLastInputInfo`) but the hook has seen nothing. Add a "Reinstall hook" tray item.
  8. **Lost key-ups.** While recording, check the physical key state (`GetAsyncKeyState`) about every 250 ms. Reset the state machine on session lock.
- **Proof it works:** an automated list of shortcuts that must still work (Win alone, Win+Shift+S, Ctrl+Win+Right, Win+V, Ctrl+C), plus "Start never opens across 50 holds".

### R2 🔴 The recording popup steals focus, so the text pastes into Wren
*Raised by: design, codebase, zoom-out, comprehension, implementation-readiness, structural. Confidence: high.*

- **What goes wrong**
  - In Tauri 2, showing a window normally activates it *(belief, high)*. If the popup becomes the foreground window, Ctrl+V lands in Wren instead of Claude, and the correction watcher reads the wrong box.
  - Creating a WebView2 window when it's needed also costs 200–500 ms.
- **What to do**
  - Create the popup once at startup and keep it hidden.
  - Set `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_TRANSPARENT` through raw Win32 (the `windows` crate). This also makes it click-through.
  - Show it with `ShowWindow(SW_SHOWNOACTIVATE)`. Never call Tauri's `show()` or `set_focus()` on it.
  - Builder options: `focused(false)`, `decorations(false)`, `transparent(true)`, `skip_taskbar(true)`, and `focusable(false)` if that option exists in the installed Tauri version.
  - Safety net: at paste time, if the foreground window belongs to Wren, don't paste. Copy the text and show "Copied, press Ctrl+V".
  - Automated test: the foreground window is the same before and after the popup appears.

### R3 🔴 Paste fires while Win is still down, or the clipboard is restored too early
*Raised by: all six. Confidence: high.*

- **What goes wrong**
  - If the owner lifts Ctrl first, Win is still held. Wren's Ctrl+V then becomes Win+Ctrl+V.
  - `SendInput` returns before Electron has read the clipboard. Restoring the old clipboard straight away makes Claude paste the *old* contents.
  - Saving and restoring only text wipes out a screenshot the owner has just taken with Win+Shift+S.
  - Every dictation also clutters Win+V clipboard history.
- **What to do**
  - Stop recording on the first key-up. Send Ctrl+V only once **both** keys are physically up, waiting at most 2 s. If they're still down after that, don't paste: save to History and show "Saved, click to copy".
  - Send Ctrl-down, V-down, V-up, Ctrl-up in **one** `SendInput` call, so no key can get stuck.
  - Save the text and image formats before pasting (`CF_UNICODETEXT`, `CF_DIBV5`/PNG). Restore them **400 ms** after the paste, and only if the clipboard sequence number shows nobody copied something new in the meantime. Accept that files, HTML and delayed-render formats are lost.
  - Set `CanIncludeInClipboardHistory=0`, `CanUploadToCloudClipboard=0` and `ExcludeClipboardContentFromMonitorProcessing` on both Wren's write and the restore. `arboard` reportedly has `exclude_from_history()` *(belief)*; otherwise use `clipboard-win`.
  - Retry `OpenClipboard` with a short back-off.
  - **Never fall back to typing the text key by key.** A typed newline is Enter, which sends the Claude message.

### R4 🔴 The speech helper won't start, leaks GPU memory, or isn't ready
*Raised by: design, codebase, zoom-out, implementation-readiness, structural. Confidence: high.*

- **What goes wrong**
  - The likeliest overnight failure is the environment, not the code. faster-whisper's engine (CTranslate2 4.5+) on Windows needs the CUDA 12 cuBLAS and cuDNN 9 DLLs. The classic error is "cudnn_ops64_9.dll not found".
  - If Wren crashes, a leftover `python.exe` keeps about 1.5–2 GB of graphics memory (VRAM).
  - The concept doesn't say how the app launches, talks to or restarts the helper ("sidecar"), or what happens if the hotkey is pressed while the model is still loading.
- **What to do**
  - **Install:** a `uv` virtual environment at `%LOCALAPPDATA%\Wren\runtime\.venv`, with Python 3.11. CUDA comes from the pip packages `nvidia-cublas-cu12` and `nvidia-cudnn-cu12`, added at startup with `os.add_dll_directory`.
    - Overseer already runs faster-whisper on this GPU. Copy its *version pins* as a recipe, but share no code, venv or model cache, because the owner wants Wren standalone.
    - No PyInstaller.
  - **Launch:** with `CREATE_NO_WINDOW` (no console flash), inside a Windows Job Object set to `KILL_ON_JOB_CLOSE`, so the helper always dies with Wren.
  - **Protocol:** JSON lines over stdin/stdout. No network port, so no firewall prompt and no clash with Overseer. The draft contract is in [Appendix B](#appendix-b-module-breakdown-and-contracts).
  - **Warm-up:** load the model, then transcribe a bundled clip about as long as the longest expected one. Only then send `ready`. This makes the first-call cost and peak memory happen at boot.
  - **Supervision:** restart after 1 s, then 2 s, then 5 s. After 5 failures in 5 minutes, show an error. Log the helper's error output to a file.
  - **Pressed before ready:** record anyway and show "Warming up…". Paste if the text is ready within 5 s and the same window still has focus. Otherwise save to History and say so.
  - **Fallback if the CUDA DLL setup won't cooperate:** an independently downloaded official whisper.cpp CUDA release (`whisper-server.exe`), behind the same `transcribe` interface. Not Willow's copy: the constraints rule out reusing Willow's parts.

### R5 🔴 Machine prerequisites that need an admin prompt
*Raised by: implementation-readiness (strongly), codebase. Confidence: high.*

- **What goes wrong**
  - Tauri and Rust need MSVC Build Tools and a Windows SDK. Installing those needs a UAC (admin) prompt that nobody is awake to click.
  - The same goes for NVIDIA driver updates (and possible reboots), and for the WiX/.NET 3.5 setup behind Tauri's MSI installer.
  - A GPU build of `whisper-rs` needs CMake, libclang and the CUDA Toolkit at build time.
- **What to do**
  - Run a **read-only** check script first:
    - `vswhere`/`link.exe`, `rustc`, `node`, `uv`/Python
    - `nvidia-smi`, which must report CUDA 12 or later
    - at least 15 GB free disk and at least 2 GB free VRAM
    - an unlocked session
  - Install only things that need no admin: rustup, `uv` (with its own Python), portable Node.
  - Use bundle target `nsis` (per-user) or `--no-bundle`. Don't build an MSI.
  - **If MSVC is missing:** stop the Rust build, still deliver the speech helper and its tests, and say so plainly in the morning note.

### R6 🔴 The build waits on the benchmark to choose a model
*Raised by: all six. Confidence: high.*

- "faster-whisper or Parakeet, chosen by benchmark" leaves builders waiting on a benchmark whose answer key is flawed (see D1).
- **Default, fixed now:** faster-whisper `large-v3-turbo`, with these settings:
  - `compute_type="int8_float16"`
  - `language="en"` (forced; this skips language detection, which costs time and sometimes guesses wrong on short clips)
  - `vad_filter=True`
  - `condition_on_previous_text=False`
  - the dictionary passed through `hotwords` plus a British-English `initial_prompt`, capped at about 50 terms
- **Beam size (Synthesis call):** reviewers split between greedy (1) and beam 5.
  - Measure the full release-to-text time on a 10 s fixture clip.
  - Use beam 5 if the typical time stays at or under 600 ms; otherwise use greedy.
- **No NeMo tonight.** NVIDIA's NeMo has no official Windows support, pulls in several GB of PyTorch, and `nemo_text_processing` needs `pynini`, which effectively won't install on Windows. If Parakeet is benchmarked, run it through ONNX (`onnx-asr` or `sherpa-onnx`).
- The benchmark runs as a separate offline tool, using the same helper protocol. It can only change the config file.

### R7 🔴 Nothing can be verified without a human, and live tests could do damage
*Raised by: implementation-readiness, structural. Confidence: high.*

- **What goes wrong**
  - Injected keys and pastes land in whatever window has focus, which could be the owner's real Claude conversations.
  - If the PC locks overnight, injected input and UI Automation both stop working, so live tests fail for the wrong reason.
- **What to do**
  - Build a **test-only Electron app** to receive pastes. It has a `<textarea>` and a ProseMirror-style editable box, which is closer to Claude's input. Tests focus it explicitly.
  - **Never send keystrokes to the Claude desktop app.** Reading Claude read-only for the UIA probe is allowed.
  - Test audio comes from WAV files, never the live mic.
    - Pipeline WAVs can be made with the built-in en-GB SAPI voice "Hazel" *(belief that it's installed)*.
    - Accuracy uses copies of Willow recordings.
  - If the session is locked, or Willow or Glaido is running, skip the live tests and say so. Don't report them as passed.
  - Add `wren --selftest`: it checks the hook installs, a mic exists, the helper is `ready`, the database is writable, and a bundled WAV goes all the way through to text. This is the build's definition of done.
  - The full list is in [§6](#6-acceptance-tests-what-working-by-morning-means).

### R8 🔴 Undecided numbers: parallel builders will invent conflicting values
*Raised by: structural, comprehension, implementation-readiness. Confidence: high.*

- None of these are in the concept:
  - the tap, hold and double-tap thresholds
  - latency targets and maximum recording length
  - the clipboard delay
  - how "3 days" is counted
  - the stats formulas
  - file locations
- Separate agents will each pick different ones.
- **What to do:** put one shared constants table in code and in the plan before work is split up. [§4](#4-open-questions-with-suggested-defaults) supplies every value.

### R9 🔴 Two hooks fire at once: Willow/Glaido still running, or two copies of Wren
*Raised by: codebase, design, zoom-out, implementation-readiness, structural. Confidence: high.*

- **Correction to the concept:** low-level hooks don't take turns. If Willow or Glaido also listens for Ctrl+Win, all of them fire, and the text is pasted twice or three times. "Must be closed" still holds, for that reason.
- **What to do**
  - Allow only one running copy of Wren (`tauri-plugin-single-instance`).
  - At startup, detect Willow and Glaido and show a banner explaining the clash. Never kill them, and never change their settings, files or registry. That way rolling back is just the owner reopening Willow.

---

## 2. Design objections (🟡 Major: fix in v1, but they won't stop the app working)

### D1. Willow's transcripts aren't a valid answer key for the benchmark
*All six lenses.*
- They are another engine's output, probably cleaned up by AI afterwards. Scoring against them measures **agreement with Willow**: a model that fixes Willow's accent mistakes loses points, and one that repeats them gains points.
- **Do instead**
  - Normalise both sides first (case, punctuation, fillers, British/American spelling) with `jiwer` plus openai-whisper's `EnglishTextNormalizer`. Call the result "disagreement with Willow", not a word error rate.
  - Report:
    - a **disagreement sheet** (clip, Willow's text, each candidate's text) for the owner to listen to, 30–50 clips
    - a **dictionary-word hit rate**, since those spellings are known to be right
    - typical (p50) and slowest-5% (p95) release-to-text time on 5, 15 and 60 s clips
    - peak VRAM
  - Optionally, where models disagree, get a tie-breaker from `large-v3` (beam 5, fp16) and flag clips where two models agree against Willow.
  - Treat a difference under about one normalised point as a tie, and prefer the engine that supports biasing (D3).
  - Use a stratified sample of about 200 clips. Recordings and results go under `%LOCALAPPDATA%`, never into git.

### D2. Learning corrections in the Claude app: risky, and harmful if it learns the wrong thing
*All six lenses.*
- **Feasibility**
  - Chromium-based apps build their accessibility tree only when an accessibility client asks. The first read often comes back empty, so retry after 200–500 ms.
  - Claude's input box is a rich-text editor. TextPattern (reading text through `DocumentRange.GetText(-1)`) probably works; ValuePattern probably doesn't *(belief)*.
  - UIA calls between processes can block for seconds while the target app is busy.
- **Speed risk**
  - Once the full accessibility tree is switched on, long Claude conversations may get noticeably slower *(belief, medium)*. That is a speed hit to the app the owner uses 98% of the time.
- **Timing**
  - Enter sends the message and empties the box, so the window for edits can be very short.
- **Wrong lessons**
  - Rewording ("Thursday" → "Friday", "good" → "great") would be learned as a mis-hearing.
  - The owner's own example, a global "Q → queue" rule, would break "Q3" and "Q&A" for good.
  - A wrong rule is silent, global and permanent, which is worse than not learning at all.
- **Do instead**
  1. **Build learning from the History screen first.** This is the path guaranteed to work tonight.
  2. **Tonight, build only a probe.** It reads (never types into) Claude's input box and reports:
     - what UIA can see
     - how long each read takes
     - whether Claude gets slower
     - a yes/no verdict for the morning
  3. **If the watcher ships, it's behind a setting.**
     - It runs on its own thread and reads only the focused element.
     - It takes a snapshot at paste, then polls every 300–500 ms.
     - It stops after 60 s, when focus moves, when the box empties (message sent) or when the next dictation starts.
     - It keeps the last snapshot that still has text.
     - It switches itself off automatically if it measurably slows Claude.
  4. **What counts as a correction** (one pure diff function, shared with History learning):
     - Inside the pasted text, a run of 1–3 words is replaced by 1–3 words, with unchanged words on each side.
     - The old and new words sound alike (Double Metaphone via `rphonetic`) or are spelt alike (within about 40% edit distance via `strsim`).
     - Ignore edits that only change punctuation or capitals, pure insertions or deletions, and anything with more than 2 separate edits (that's a rewrite).
  5. **Terms and Replacements.** See D3 and the defaults table.

### D3. The dictionary mechanism depends on the engine, and on keeping "terms" separate from "replacements"
*Design, codebase, zoom-out, comprehension, structural.*
- **"Bias the model with the dictionary" only works on some engines.**
  - Whisper has a prompt capped at about 224 tokens (`initial_prompt`, plus `hotwords` in faster-whisper). Long prompts make it echo or invent words.
  - Parakeet through ONNX has no simple equivalent. NeMo phrase boosting exists only inside Python NeMo *(belief)*. If Parakeet ever wins, the dictionary can only fix words after transcription.
- **Two entry types, kept apart from the first database schema:**
  - **Term:** a soft nudge, sent to the model. A bad one does little harm.
  - **Replacement (from → to):** whole-word, case-aware, applied after transcription, for any engine. A bad one corrupts every future dictation.
- **Record where every rule came from and what it changed.**
  - Each transcription stores its raw text, its final text and the IDs of the rules applied.
  - History underlines words a rule changed, with "Stop correcting this".
  - Editing the word back disables the rule automatically.
- **Send the model a ranked, capped bias list** of about 50 terms: manual and learned terms first (by hits and how recent they are), then the Willow seed.

### D4. Silence and accidental taps paste made-up text
*All six.*
- On near-silent clips Whisper outputs "Thank you.", "you" or the prompt words. On noise it repeats itself. These would paste straight into Claude.
- **Do**
  - Gate with Silero VAD (voice activity detection, which finds the parts with speech; faster-whisper's `vad_filter` already includes it).
  - With under 300 ms of speech, don't call the model and paste nothing.
  - When there's under 2 s of speech, drop known silence phrases ("Thank you.", "Thanks for watching", a lone "you").
  - Collapse any phrase repeated more than 4 times, and flag it in History.
  - Never paste empty text.

### D5. Microphone: clipped first words, and where audio is captured
*Design, codebase, zoom-out, comprehension, structural.*
- Opening a capture stream on key-down can take 50–300 ms, more on Bluetooth, so the first word gets clipped *(belief on the figures)*.
- **Synthesis call:** the reviewers split three ways (always open with a 300 ms pre-roll; open on key-down; set up at startup and start on key-down).
  - **Default:** set up the device at startup and start the stream on chord-down. Starting is about 10–30 ms *(belief)*, and the privacy light then shows only while recording.
  - Measure clipping. If the first word is clipped, offer an always-open stream with a 300 ms pre-roll as a setting.
  - Note that always-open keeps the mic light on, and pushes Bluetooth headsets into low-quality call mode.
- **Capture in Rust, not Python (Synthesis call).**
  - The implementation-readiness lens preferred capturing in the helper, which is simpler.
  - Design, codebase and structural preferred Rust (`cpal` with `rubato` resampling to 16 kHz mono, written with `hound`). Recording then starts instantly whatever Python is doing, and a helper crash never loses a dictation.
  - Rust writes the WAV file first and sends its path to the helper. That one file also serves 3-day retention and retry-after-crash.
- **Mic state shown to the user**
  - Follow changes to the Windows default input.
  - If no audio, or only near-silence, arrives within about 300 ms to 1.5 s of key-down, show "No sound from mic". The Windows mic privacy switch delivers silence rather than an error *(belief)*. Clicking the message opens `ms-settings:privacy-microphone`.

### D6. Graphics memory is tight, and running out fails silently
*Design, codebase, zoom-out, implementation-readiness, structural.*
- **The budget** (figures are beliefs):

  | What | VRAM |
  | --- | --- |
  | Free on the card | about 4.6 GB (8 GB minus 3.4 GB at idle) |
  | turbo in int8 + CUDA overhead | about 1.5–2 GB |
  | Overseer's faster-whisper, if loaded | 1–3 GB |
  | The optional AI clean-up | about 3 GB |

  Wren plus Overseer fits. Adding the AI clean-up probably doesn't.
- When VRAM runs out, the NVIDIA driver can quietly spill into system RAM instead of raising an error. Dictation then becomes 5–20× slower with no message *(belief)*.
- **Do**
  - Set a speech budget of 2.5 GB or less.
  - Log free VRAM (via NVML) at startup and with every transcription.
  - Warn when processing time per second of audio is more than double the warm baseline.
  - Report out-of-memory as its own error.
  - Never unload the speech model when idle; reloading costs seconds.
  - **Synthesis call on the CPU fallback:** use one (`small.en`, int8) but **never silently**. The popup and the log say "running on CPU".
  - **Owner action for the morning:** in NVIDIA Control Panel, set "CUDA – Sysmem Fallback Policy" to "Prefer No Sysmem Fallback" for Wren's `python.exe`.

### D7. Long hands-free dictations: slow to finish, and easy to leave running
*Design, comprehension, structural.*
- Transcribing a 5-minute recording in one go on release could take about 4–10 s *(belief)*.
- **Do**
  - Tonight, transcribe the whole clip on release (simplest), but make the protocol piece-based now.
  - Measure a 3-minute fixture. If release-to-text is over 1.5 s, cut the audio at pauses (more than about 600 ms of silence, or 25 s at most). Transcribe the pieces in the background, passing the earlier text as the prompt, so only the last piece is left at stop.
  - **Caps:**
    - Hands-free and hold both stop at 10 minutes, with a warning pulse at 9. At the cap, stop and paste if focus hasn't moved.
    - After 90 s of continuous silence, or on lock or sleep, stop and save to History without pasting, then show "Hands-free stopped, saved".
  - The × on the popup discards the recording.

### D8. The speed budget isn't written down or measured
*Design, comprehension, structural.*
- **Targets**
  - Popup visible 120–150 ms after chord-down. The 120 ms delay means Ctrl+Win+Left never flashes the popup.
  - From release to text, for a 10 s clip: typical (p50) ≤ 600 ms, slowest 5% (p95) ≤ 1.2 s.
- **Rough budget for one dictation** *(belief)*:

  | Step | Time |
  | --- | --- |
  | Detect the release | under 1 ms |
  | Trim silence (VAD) | 10–30 ms |
  | Transcribe with turbo | 150–400 ms |
  | Tidy-up | under 1 ms |
  | Clipboard + Ctrl+V | 10–30 ms |
  | Electron shows the text | 20–50 ms |

- **Do:** store a timestamp for every stage in each history row and in the log, so a slowdown can be pinned to one step.

### D9. The user can't see what's happening or what failed
*Comprehension, design, codebase.*
- **Minimum popup states**

  | State | What the popup shows |
  | --- | --- |
  | Listening | Level bars driven by the real mic signal, shown only once audio arrives (proof that it hears you) |
  | Hands-free | A different colour, an elapsed timer, "tap Ctrl+Win to finish", and × to discard |
  | Warming up | "Warming up…" |
  | Transcribing | A shimmer, then "Still working…" after 1.5 s |
  | Pasted | A tick, hidden after 400 ms |
  | Nothing heard | "Didn't catch that" for 1.5 s |
  | Paste failed | "Not pasted, click to copy" for 4 s |
  | Error | A short reason (engine down, out of GPU memory, no mic, running on CPU) for 5 s; clicking opens Wren |

- **Every dictation is saved to History before the paste is attempted.** Copying from History is the guaranteed recovery.

### D10. The stats need hourly buckets from day one
*Zoom-out, comprehension, implementation-readiness, structural.*
- "Busiest hour" can't be worked out from daily totals once the text is deleted, and data that was never recorded can't be backfilled.
- **Do**
  - Keep `hourly_totals(local_date, hour, dictations, words, speaking_ms)` forever.
  - Update it in the **same transaction** as the transcription insert, not in the deletion job, which could crash.
  - Daily figures, the streak, the busiest hour and the chart are all worked out from this one table.
  - Store times in UTC alongside the local date, so the BST clock change doesn't break streaks.
- **Formulas:** see the defaults table in §4.

### D11. The filler tidy-up must be conservative
*Comprehension, codebase, zoom-out, implementation-readiness, structural.*
- Removing "like" or "you know" without a model changes meaning ("I like it"). A loose regex for "er" would damage "her" and "error".
- **Do:** run an ordered list of pure steps, each with its own input/output test files:
  1. silence guard
  2. whitespace
  3. filler removal (list in config, whole words only)
  4. lowercase "i" → "I"
  5. Replacements
  6. [optional AI clean-up]
  7. Replacements again
  8. final spacing
- The word lists are in §4.

### D12. Focus moves during transcription, and admin windows
*Comprehension, design, implementation-readiness, codebase.*
- **Paste target:** the target is the window that has focus at stop. If a different window has focus when the paste is due, don't paste; show "Not pasted, click to copy".
- **Admin windows:** Windows' integrity rules (UIPI) mean Wren's hook doesn't see keys, and pastes don't arrive, when an admin (elevated) window has focus. Accept this. Detect it where possible and copy instead.
- **Overlapping dictations:** a chord pressed while the previous dictation is still transcribing is allowed. Results are pasted in order.

### D13. Logs and diagnosis
*Structural, codebase, design.*
- Write JSON-lines logs to `%LOCALAPPDATA%\Wren\logs` (`tracing` + `tracing-appender`).
- **One record per dictation**, carrying a dictation ID through every component:
  - stage timings
  - the model used
  - how much speech the VAD found
  - the foreground app's process name
  - the paste outcome
- **Also log:** hook health (time of the last event seen, re-installs), the helper's error output, and UIA probe success and failure counts.
- **No dictated text in logs.** Otherwise the 3-day deletion rule is broken.

### D14. The optional AI clean-up: memory, and answering instead of cleaning
*Zoom-out, design, codebase, comprehension, implementation-readiness, structural.*
- **The objection**
  - It doesn't fit in VRAM alongside the speech model and Overseer (D6).
  - It adds a third runtime.
  - Small local models tend to **answer** a dictated prompt instead of tidying it, and 98% of dictation is prompts to Claude.
- **Synthesis call:** the zoom-out lens wanted it cut entirely, switch included. But the owner listed it as in scope, so:
  - Keep an empty pipeline step for it, and a switch that is OFF.
  - Build it **last**, only if everything else passes its acceptance tests.
- **If it's built**
  - Use the prebuilt llama.cpp CUDA `llama-server.exe` on 127.0.0.1, with a 3–4B model at Q4 (for example Qwen2.5-3B).
  - Load it only when the switch is on and enough VRAM is free.
  - Wrap the text in delimiters, with instructions to fix punctuation, capitals and obvious mis-hearings only, and never to answer.
  - If a reply takes over 2.5 s, or the length changes by more than 30%, use the tidied text instead.

### D15. Defender and packaging
*Implementation-readiness.*
- To antivirus heuristics, an unsigned exe with a keyboard hook looks like a keylogger, and PyInstaller bundles are often wrongly flagged.
- Files built locally don't carry the "downloaded from the internet" mark, so SmartScreen shouldn't prompt. Defender quarantining the exe is still a morning risk.
- **Do:** no PyInstaller, no MSI. Run the built exe directly or use a per-user NSIS installer. The morning note says how to check Defender's quarantine.

### D16. The concept doc isn't yet a build spec
*Structural, comprehension.*
- **Missing**
  - numeric targets
  - error states and what the user sees
  - a data model
  - acceptance criteria
  - a glossary (tap, hold, hands-free, term, replacement, time saved)
  - file locations
  - alternatives considered
  - a mitigation for each risk
  - what ships by morning versus later
- **In the wrong place**
  - The benchmark is a work plan, not architecture.
  - The hardware belongs under environment/targets.
  - "Willow and Glaido must be closed" is an operating rule, not a risk.
  - "Improve the animation" needs a measurable criterion, or should go on the polish list.
- **Ambiguities two builders would build differently:**
  - "double-tap (or single tap) again to stop": which is it?
  - Which filler words exactly?
  - How are "time saved" and words per minute calculated?
  - What does "audio" retention mean?
  - What does "big" mean in numbers?
  - What happens when the paste target isn't a text box?

  Each is answered in §4.

---

## 3. Simpler alternatives worth taking

1. **Mine Handy before writing from scratch, with a one-hour time box.** *(Codebase, zoom-out.)*
   - Handy (cjpais/Handy, MIT) is reported to use Tauri 2 + React/TS/Vite, `cpal` audio, Silero VAD, Whisper through `whisper-rs`, Parakeet through ONNX, push-to-talk, an overlay popup and paste *(belief on its current features)*.
   - Its shortcut system almost certainly can't do a bare Ctrl+Win chord with Start masking, so the hook gets written anyway.
   - Its GPU Whisper build needs native build tools (see R5).
   - **Default:** build Wren's own shell, and copy Handy's overlay, audio and paste code where it helps, keeping its MIT notice.
   - Optionally leave Handy installed as a morning fallback, if it builds.
   - Other references to mine for ideas: Whispering/Epicenter (Tauri + Svelte), OpenWhispr (Electron), WhisperWriter (Python).
2. **One engine tonight: faster-whisper in a `uv` venv.** The benchmark code and the shipped code are the same helper. Use the official whisper.cpp CUDA `whisper-server.exe` as the fallback (R4). Parakeet appears only in the benchmark, through ONNX.
3. **Keep speech out of the Rust process.** A GPU crash would otherwise take down the hook in the middle of a chord. GPU-enabled Rust speech crates (`whisper-rs`, `ct2rs`) are also the riskiest thing to compile unattended.
4. **stdin/stdout JSON lines, not localhost HTTP.**
5. **History-screen learning first.** The UIA watcher is a probe tonight and a setting later.
6. **Learned words start as Terms only.** Replacements come later and need guards.
7. **Stop hands-free with a single tap.** A double-tap to stop would add a waiting window. "Double-tap to stop" is dropped.
8. **Plain `LIKE` search instead of an FTS5 full-text index.** Three days of history is a few hundred rows.
9. **One hourly-totals table drives every stat.** Use CSS bars or Recharts, not a heavy chart library.
10. **The Willow dictionary import is a one-off script with a neutral format** (CSV, JSON or one term per line), not an in-app feature.
    - Add a Willow-specific adapter only if the parallel Willow study finds the file and its format.
    - Copy the source file first, never write to Willow's files, remove duplicates regardless of case, and make the import undoable by batch.
11. **No installer, no PyInstaller, no audio playback screen.**
12. **Transcribe the whole clip on release.** Chunking only if the measurement in D7 says so.

**Proposed build order for v1** (merged from the zoom-out and structural lenses)

1. Prerequisite check (R5), the shared constants table (R8) and the helper protocol, frozen before work is split up.
2. Hook + pure chord state machine + table tests (R1).
3. Speech helper: venv, warm-up, `ready`, supervision, Job Object (R4/R6).
4. Hold-to-talk end to end: Rust audio → WAV → helper → tidy-up → paste only after both keys are up → delayed clipboard restore (R3, D4, D11).
5. The non-activating popup with all its states (R2, D9).
6. Tray icon, single instance, Willow/Glaido warning, logs, `--selftest` (R9, D13).
7. SQLite: transcriptions + audio files, hourly totals, the 72-hour sweep (D10).
8. Home page (big stats, click-to-copy cards) and History search.
9. Double-tap hands-free with its caps (D7).
10. Dictionary: Willow seed as Terms, Replacements, bias list, History-screen learning (D3).
11. Settings: mic picker, dictionary list, start-with-Windows toggle.
12. Morning reports: the benchmark disagreement sheet, and the UIA probe verdict for Claude.
13. Polish: chart, streak and busiest-hour screens, animation, and the AI clean-up if there's time.

Items 1–6 are what "working by morning" depends on.

---

## 4. Open questions with suggested defaults

The owner can't be asked tonight; these are the defaults to build with. Where reviewers disagreed, the losing option is noted.

### Hotkey and timing

| Question | Default |
| --- | --- |
| Tap vs hold | A release under **250 ms** is a tap (audio thrown away). 250 ms or more is a hold: transcribe at once, with no added wait. |
| Double-tap gap | A second chord-down within **350 ms** of the first release starts hands-free. (One reviewer said 400.) |
| Stopping hands-free | **Single chord press.** Further chord presses are ignored for 500 ms. |
| Key order | Either order, left-hand keys only. |
| Another key during a hold | Cancel, throw the audio away, let the shortcut through. No other way to cancel exists (no extra hotkeys). |
| Other keys during hands-free | Don't cancel; the owner may be typing. |
| Another key after 3 s or more of holding | Save to History, don't paste. |
| When the mask key is injected | At chord recognition, while Win is held. Verify against injecting at release (R1). |
| Popup delay | Shown 120 ms after chord-down unless another key has been pressed. |
| Start/stop sounds | Off, with a setting. |

### Speech

| Question | Default |
| --- | --- |
| Engine and model | faster-whisper `large-v3-turbo`, `int8_float16`, `language="en"`, VAD on, `condition_on_previous_text=False`. |
| Beam size | Measured: 5 if a 10 s clip's release-to-text p50 stays ≤ 600 ms, otherwise 1. |
| Switching engine | Only if a candidate wins by more than one normalised point without losing dictionary hits. Parakeet only through ONNX. |
| No CUDA or out of VRAM | CPU `small.en` int8, with a visible warning. Never silent. |
| Model storage | `%LOCALAPPDATA%\Wren\models`, pre-downloaded by the build. No download screen. |
| Overseer at the same time | Assume yes, and size VRAM for both. |

### Microphone

| Question | Default |
| --- | --- |
| Which device | The Windows default input, following changes. A mic picker in settings. |
| Open when | Device set up at startup, stream started on chord-down. Always-open with a 300 ms pre-roll only if first words are measurably clipped. |
| Where it's captured | In Rust, written to a 16 kHz mono WAV. (One reviewer preferred capturing in the helper.) |

### Paste

| Question | Default |
| --- | --- |
| When | Once both keys are physically up, waiting at most 2 s; otherwise save to History with "click to copy". |
| Target | The foreground window at stop. If it changed, or is Wren or an admin window: don't paste, copy, and show a notice. |
| Clipboard restore | Text and image formats, 400 ms after the paste, only if the sequence number is unchanged. Wren's writes kept out of Win+V history. (Reviewers ranged from 150 to 500 ms; measure on the test Electron app.) |
| Space before or after the pasted text | None tonight. (One reviewer wanted a trailing space with a setting.) |
| Typing fallback | Never. |

### Hands-free

| Question | Default |
| --- | --- |
| Maximum length | 10 minutes (warning at 9), then stop and paste. Holds are capped at 10 minutes too. |
| Silence auto-stop | 90 s of continuous silence → stop, save to History, don't paste. |
| Lock or sleep while recording | Stop, save to History, don't paste. |

### Text tidy-up

| Question | Default |
| --- | --- |
| Fillers removed | um, umm, uh, uhh, er, err, erm, ermm, hmm. Whole words only, any case, but not all-caps (so "ER" survives). Then remove orphaned commas and re-capitalise. |
| Never removed | like, you know, I mean, so, well, basically, actually, ah, yeah, mhm. Repeated words are not collapsed ("had had" is valid). |
| "i" fixes | i, i'm, i've, i'll, i'd → capitalised. |
| US → UK spelling | **No conversion pass.** Write the Whisper prompt in British English instead. Two reviewers wanted a conversion map with exclusions; three said it would break code words like CSS `color` in Codex. Offer it as an off-by-default setting later. |
| Numbers | Leave them as Whisper writes them (digits). |

### Dictionary and learning

| Question | Default |
| --- | --- |
| Entry types | **Term** (nudges the model) and **Replacement** (from → to, whole-word, case-aware). |
| Correcting in History | **Synthesis call:** a dedicated "Fix a word" action (select the word, type the right one) rather than free-text editing. It's unambiguously a correction, so it becomes an active Replacement at once. (Free-text editing would have the same rewording problem as the watcher.) |
| Auto-detected in the text box | Becomes a Term at once. It becomes a Replacement only after 2 identical sightings that sound or look alike. Never automatically for words of 2 characters or fewer, or for common English words. |
| Showing a learned correction | A non-focusing toast, "Learned: X → Y · Undo", for 6 s. A Learned list on the Dictionary page showing times applied, with delete. |
| Watch window | Polls every 300–500 ms. Stops after 60 s, when focus leaves, when the box empties, or when the next dictation starts. (Reviewers ranged from 30 to 120 s.) |
| UIA watcher setting | Off tonight. The probe runs read-only and writes a verdict. |
| Forcing accessibility on in Claude | No. Rely on retries, and log the success rate. |
| Willow seed | Imported once, as active **Terms**, source `willow_seed`, undoable by batch. |
| Bias list size | About 50 terms, ranked by source and hits, within the about-224-token prompt limit. |

### Stats

| Question | Default |
| --- | --- |
| Words | Whitespace-separated words in the final text. Only dictations with at least one word count, pasted or not. |
| Words per minute | Words ÷ minutes of speaking (chord down to stop). |
| Time saved | max(0, words ÷ 40 − speaking minutes). The 40 wpm typing speed is a setting. |
| Day streak | Consecutive local days with at least one dictation, ending today or yesterday. |
| Busiest hour | The local hour with the most words over the last 30 days. |
| Chart | Words per day for the last 30 days. |
| Day boundary | Local midnight. |

### Storage and app behaviour

| Question | Default |
| --- | --- |
| "3 days" | A rolling 72 hours from creation. The sweep runs at startup and hourly, deletes the audio file first and then the row, and removes orphan audio files. |
| Audio kept | 16 kHz mono WAV per dictation (about 2 MB/min). No playback screen tonight. |
| Text kept | Raw, tidied and final versions, plus the IDs of rules applied. |
| Search | Plain `LIKE`. (One reviewer wanted FTS5.) |
| Database | `rusqlite` (bundled) owned by Rust, not `tauri-plugin-sql`. WAL mode, `PRAGMA user_version` migrations, `secure_delete` on. |
| File locations | Settings: `%APPDATA%\Wren\settings.json` (versioned, written atomically). Database, audio, models, runtime and logs: `%LOCALAPPDATA%\Wren\{db,audio,models,runtime,logs}`. |
| Start with Windows | **Off tonight**, with a settings toggle. **Synthesis call:** four reviewers said on, two said off. Off wins, because the next reboot would otherwise start Wren *alongside* Willow and Glaido if they also start with Windows, causing double pastes. The morning note tells the owner to turn it on after disabling theirs. |
| Close button | Hides to the tray; Quit lives in the tray menu. |
| Willow or Glaido running | Warn; never kill them or change their settings. |
| Logs containing dictated text | Never. |

---

## 5. Polish items (🟢)

- **Popup placement:** on the monitor showing the focused window (falling back to the one with the mouse cursor), centred horizontally, 24 px above the bottom of that monitor's work area, DPI-aware per monitor. Take the size from the parallel Glaido study; the fallback is a 200×52 px pill. A static pill with live level bars tonight, with animation improvements later.
- **"Big" UI starting numbers:**
  - window 1200×800 (minimum 960×640)
  - text: body 16 px, card text 18 px, stat figures 44 px semibold, labels 14 px
  - cards: 20 px padding, 14 px corner radius
  - Home: 4 stats and the 20 most recent cards
  - take the light theme from the parallel Willow study
- **Card clicks:** clicking copies the text ("Copied" for 1 s). Edit and "Fix a word" are hover icons, so they don't clash with click-to-copy.
- **Streak, busiest-hour and chart screens:** can come late, because the data is recorded from night one (D10).
- **US → UK spelling map** as an off-by-default setting, built from openai-whisper's `normalizers/english.json` reversed. Exclude ambiguous pairs (program/programme, license/licence, practice/practise, check/cheque, meter/metre, tire/tyre) and code-like words.
- **Long pastes:** Claude desktop may turn a very long paste into an attachment *(belief, low-medium)*. Test with a long hands-free dictation.
- **Code tidiness:** generate TypeScript types from the Rust commands (`ts-rs` or `specta`).
- **Startup:** the hotkey hook installs and stays alive with the window hidden. An optional `tauri-plugin-autostart` later, for the start-with-Windows toggle.
- **Owner actions for the morning:**
  - the NVIDIA Sysmem Fallback setting (D6)
  - hand-label about 50 benchmark clips
  - disable Willow and Glaido autostart, then enable Wren's

---

## 6. Acceptance tests: what "working by morning" means

Agent-verifiable unless marked otherwise.

1. **Prerequisite check:** the check script exits with code 0 and saves its report.
2. **Speech helper**
   - Sends `ready` with `device=cuda` within 60 s.
   - Fixture WAVs of 5–15 s transcribe in a median under 700 ms.
   - A silent WAV and a 0.2 s click both return empty text.
   - Killing the helper brings it back within 5 s.
   - Closing Wren leaves no Wren `python.exe` running.
3. **Dictionary:** a Hazel (SAPI) WAV containing an invented name comes out right with the dictionary entry and wrong without it, and a Replacement rule is applied.
4. **Tidy-up:** table-driven unit tests for every step.
5. **Chord state machine:** a pure-function test fed key-event lists, with no OS involved. Cases:
   - hold, tap, double-tap, stopping hands-free
   - Win pressed first, auto-repeat
   - Ctrl+Win+→ cancelling the recording
   - Wren's own tagged keys being ignored
   - every key always being passed on
6. **Live test** (only with the session unlocked and Willow/Glaido not running; skipped and reported otherwise):
   - An injected 2 s Ctrl+Win hold with a fake-mic WAV puts the expected text into the test app's textarea and its editable box.
   - The foreground window is unchanged by the popup.
   - After release, neither `StartMenuExperienceHost` nor `SearchHost` is in the foreground, across 50 tries.
   - Ctrl+Win+Right, Win+Shift+S and Win+V still work.
   - Typing passes through while Wren runs.
   - A clipboard image survives a paste (checked by comparing hashes).
   - The hook still responds after 10 minutes and after a heavy transcription.
7. **UIA:** read the test app's editable box, change a word with injected keys, and a "learned" Term appears. If the Claude desktop app is open, run the **read-only** probe and report what it sees and whether Claude slowed down.
8. **History and stats:**
   - Seed rows with old timestamps; the sweep removes rows and files older than 72 hours.
   - Hourly totals remain, and search finds the text.
   - Stats match hand-calculated values.
9. **UI:** Playwright against the Vite build with Tauri calls mocked, with screenshots at 1100×700 saved for the owner.
10. **Self-test:** `wren --selftest` passes.
11. **Only the owner can check, in the morning:**
    - the real physical hotkey and the Start menu
    - British-accent accuracy, live
    - a real paste and correction learning in the Claude desktop app
    - how the popup feels

---

## 7. Beliefs to verify in the first hour

None of these were checked tonight; each should be tested before a design relies on it.

- **Start-menu mask timing:** whether injecting at chord-down or at release reliably stops Start from opening.
- **Win+Ctrl+V:** whether it really opens the Windows 11 sound-output panel.
- **Tauri 2 popup:** whether `show()` activates the window, and whether `focusable(false)` exists in the installed version.
- **arboard:** whether `exclude_from_history()` exists; otherwise use `clipboard-win`.
- **faster-whisper:** how `hotwords` and `initial_prompt` behave together.
- **The working CTranslate2/cuDNN versions on this PC,** taken from Overseer's environment as a recipe only.
- **The figures:** every latency and VRAM number in this report is an estimate. Measure on this RTX 3060 Ti.
- **Chromium/Claude desktop:**
  - whether TextPattern works on the input box
  - whether the first read comes back empty
  - whether the accessibility tree makes Claude slower
- **Handy:** its current features, licence, and whether it builds on this machine.
- **The "Hazel" en-GB SAPI voice:** whether it's installed.
- **Long pastes:** whether Claude converts them into attachments.

---

## Appendix A: where each lens's main contribution landed

| Lens | Main contribution |
| --- | --- |
| design | Latency and VRAM budgets, mask-key timing, hands-free chunking, the "disagreement with Willow" framing, the GPU fallback policy |
| codebase-details | Library choices (`windows`, `cpal`, `rubato`, `hound`, `arboard`/`clipboard-win`, `uiautomation`, `similar`, `rphonetic`, `strsim`, `rusqlite`, `tracing`, `win32job`), the NeMo-on-Windows warning, Handy, the whisper.cpp fallback, spelling-map exclusions |
| zoom-out | Cut and defer list, the build order, the hidden scope (tray, single instance, mic picker, no installer), the stats data being needed on night one |
| comprehension | Exact timings, popup feedback states, filler lists, stats formulas, promotion rules for learned corrections, UI numbers |
| implementation-readiness | Admin-only prerequisites, the helper launch/protocol/supervision, the test Electron app, the locked-session hazard, Defender, the acceptance tests |
| structural | Thin hook + pure state machine + one session owner, the Term/Replacement data model with where each rule came from, the pure pipeline, hourly totals in the same transaction, logging and `--selftest`, the gaps in the spec |

## Appendix B: module breakdown and contracts

- **Main path:**
  `hotkey` (hook + pure chord machine) → `session` (sole owner of the dictation state and dictation ID; pastes results in order) → `audio` (start/stop → WAV path + duration; level events at about 30 Hz) → `engine` client ↔ speech helper → `pipeline` (ordered pure steps) → `paste` → `store`.
- **Running alongside:** `dictionary` and `watcher`. The watcher only proposes `CorrectionCandidate`s; the dictionary decides what to do with them.
- **Also:** `ui` (main window + popup) only displays state and calls commands; `diag` holds logging and the self-test.
- No module calls another sideways. Everything goes through `session` or events.

**Helper protocol** (JSON lines over stdin/stdout, `v:1`):

| Direction | Message |
| --- | --- |
| helper → app | `ready{engine, model, device, vram_mb, caps{prompt, hotwords, vad, word_probs}}` |
| app → helper | `transcribe{id, audio_path, language:"en", bias_terms[], prompt}` |
| helper → app | `result{id, text, words[{w,start,end,p}], infer_ms}` |
| helper → app | `error{id, code: OOM\|BAD_AUDIO\|INTERNAL, msg}` |
| either way | `ping` / `pong`, `shutdown` |

The `caps` block makes biasing optional, so a Parakeet or whisper.cpp engine can be swapped in later without changing the app.

**Tables:**

| Table | Columns |
| --- | --- |
| `transcriptions` | `id, created_at_utc, local_date, mode, audio_path, duration_ms, raw_text, tidied_text, final_text, words, model, timings_json, target_process, paste_outcome, applied_rule_ids` |
| `dictionary_entries` | `id, kind(term\|replacement), from_text?, to_text, source(willow_seed\|manual\|learned_paste\|learned_history), status(candidate\|active\|disabled), observations, hits, origin_transcription_id, batch_id, created_at` |
| `correction_events` | `transcription_id, from, to, source, decision`; deleted along with their transcription |
| `hourly_totals` | `local_date, hour, dictations, words, speaking_ms`; kept forever |
| `schema_version` | via `PRAGMA user_version` |
