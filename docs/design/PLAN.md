# Wren Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** A free, local Windows dictation app: hold Left Ctrl + Left Win, talk, release → text pasted into
the focused app; double-tap for hands-free; local GPU speech-to-text; dictionary that learns; 3-day history;
stats; a light, big, minimal UI.

**Architecture:** Tauri 2 app. Rust owns the keyboard hook, audio capture, text pipeline, paste, SQLite
store and windows. A Python sidecar (faster-whisper on CUDA) does speech-to-text over JSON lines on stdio.
React + TypeScript front end with two pages: the main window and a never-focused popup.

**Tech Stack:** Rust 1.98 (MSVC), Tauri 2.12, `windows` 0.61, cpal 0.18, rubato 5, hound 3.5,
rusqlite 0.41 (bundled), tracing; Python 3.12 via uv, faster-whisper + nvidia cuBLAS/cuDNN cu12 wheels;
React 19, TypeScript 6, Vite 8.

**Spec:** `docs/design/SPEC.md` (read it fully before your task). Research: `docs/research/*.md`.
Goldfish review: `docs/design/goldfish-brainstorm-report.md`.

## Global Constraints

- Every timing/threshold comes from `src-tauri/src/constants.rs`. Never hard-code one elsewhere.
- The keyboard hook never swallows a key: always `CallNextHookEx`.
- The popup never takes focus: never call `show()`/`set_focus()` on it; use `popup::show`.
- Never type text key by key. Paste only via clipboard + one tagged `SendInput` Ctrl+V.
- Never send keystrokes to the real Claude desktop app in tests. Live tests target the test page only.
- Logs never contain dictated text.
- Nothing contacts the internet at runtime (model files are pre-downloaded; Ollama is local).
- Never copy Willow or Glaido code. Handy (MIT, `.research-cache/handy`) may be adapted with an MIT notice.
- Never modify, kill or write to Willow's or Glaido's files, settings or processes.
- British English in all UI copy. Plain, short words.
- Tests use `WREN_HOME` (temp folder) so they never touch real data.
- Commit after each green step with a conventional message and the trailer
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. Ctrl+Win+Right / Win+Shift+S / Win+V / Ctrl+Win+Enter pressed while Wren runs → the Windows shortcut works
   and Wren neither records nor pastes. (Task 2 table tests + Task 8 live test.)
2. User lifts Ctrl first and holds Win for a moment → no Win+Ctrl+V; paste waits for both keys up. (Task 5.)
3. Silent or 0.2 s accidental hold → nothing pasted, no "Thank you." (Task 3 + Task 5 tests.)
4. The clipboard held an image (screenshot) before dictation → still there afterwards. (Task 5 test.)
5. Wren quits or crashes → no orphaned `python.exe` holding GPU memory. (Task 3 test.)

---

### Task 1: Foundation (DONE by the coordinator)

**Files:** `src-tauri/src/{lib.rs, constants.rs, paths.rs, settings.rs, logging.rs, popup.rs, tray.rs,
commands.rs}`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`, `vite.config.ts`,
`index.html`, `popup.html`, `src/{main.tsx, App.tsx}`, `src/popup/{main.tsx, Popup.tsx}`,
`src/styles/tokens.css`, `scripts/check-prereqs.ps1`.

**Produces:** `constants::*`; `paths::Paths { roaming, local }` with `resolve()`, `under(root)`,
`settings_file()`, `db_file()`, `audio_dir()`, `models_dir()`, `runtime_dir()`, `runtime_python()`,
`logs_dir()`, `ensure_dirs()`; `settings::Settings` (+ `EngineSettings`, `AiCleanupSettings`) with
`load(path)`/`save(path)`; `popup::{create, show, hide, set_clickable, is_own_window, emit_state,
emit_level, POPUP_STATE_EVENT, POPUP_LEVEL_EVENT}`; `tray::{show_main, EVENT_PAUSE_TOGGLED,
EVENT_REINSTALL_HOOK}`; `AppState { paths, settings: RwLock<Settings> }`; commands `get_constants`,
`app_version`, `get_settings`, `save_settings`. Cargo.toml already lists every crate the plan needs —
add a dependency only if truly required, and say so in your report.

---

### Task 2: Hotkey

**Files:** Create `src-tauri/src/hotkey/{mod.rs, chord.rs, hook.rs}`; modify `lib.rs` (module line only).
Test: `chord.rs` unit tests; `src-tauri/examples/hotkey_probe.rs` (live check tool).

**Interfaces — Produces:**
```rust
// chord.rs (pure, no OS calls)
pub enum KeyDir { Down, Up }
pub struct KeyEvent { pub vk: u16, pub dir: KeyDir, pub injected: bool, pub tagged: bool, pub t_ms: u64 }
pub enum ChordOutput { MaskNow, HoldStart, HoldEnd { held_ms: u64 }, Tap,
                       HandsFreeStart, HandsFreeStop, Cancel { held_ms: u64 } }
pub struct ChordMachine;  // new(), on_key(KeyEvent) -> Vec<ChordOutput>, on_tick(now_ms) -> Vec<ChordOutput>,
                          // force_idle(), is_recording() -> bool
// mod.rs
pub enum HotkeyEvent { HoldStart, HoldEnd { held_ms: u64 }, Tap, HandsFreeStart, HandsFreeStop,
                       Cancel { held_ms: u64 } }
pub struct HotkeyService;  // start(tx: crossbeam_channel::Sender<HotkeyEvent>) -> anyhow::Result<Self>,
                           // reinstall(&self), force_idle(&self), last_event_age_ms(&self) -> u64,
                           // set_paused(&self, bool)
pub fn chord_keys_physically_up() -> bool;              // GetAsyncKeyState on 0xA2 and 0x5B
pub fn send_tagged_keys(keys: &[(u16, bool /*down*/)]); // one SendInput call, dwExtraInfo = INJECT_TAG
```
- [ ] **Step 1: Table tests for `ChordMachine`** (spec §3 rules, constants from §2). At minimum:
  `hold_start_on_chord_down_either_order`, `hold_end_after_250ms`, `tap_under_250ms_discards`,
  `double_tap_within_350ms_starts_handsfree`, `second_tap_after_350ms_is_new_hold`,
  `handsfree_stops_on_next_chord_after_500ms_debounce`, `other_key_during_hold_cancels`
  (Shift, Alt, Right, Enter, S), `other_keys_during_handsfree_do_not_cancel`, `right_ctrl_or_right_win_never_chord`,
  `autorepeat_downs_ignored`, `tagged_events_ignored`, `untagged_injected_events_accepted`,
  `mask_emitted_on_every_chord_down`, `force_idle_resets`, `lost_keyup_recovered_by_on_tick` (via a
  key-state probe closure or explicit `on_key_state(ctrl_down, win_down, now)` method).
- [ ] **Step 2: Run** `cargo test hotkey` → FAIL. **Step 3: implement** `chord.rs`. **Step 4:** PASS.
- [ ] **Step 5: `hook.rs`** — dedicated thread, `SetWindowsHookExW(WH_KEYBOARD_LL)`, `GetMessageW` loop.
  Callback: build `KeyEvent` (injected = `LLKHF_INJECTED`, tagged = `dwExtraInfo == INJECT_TAG`), feed the
  machine, post outputs to a non-hook thread, **always** `CallNextHookEx`. Inject `VK_MASK` (tagged) on
  `MaskNow` from outside the callback, while Win is still held. Timer thread: `on_tick` every
  `KEYSTATE_POLL_MS` while recording. Watchdog: reinstall on resume/unlock (`WTSRegisterSessionNotification`,
  `WM_POWERBROADCAST`) and on `reinstall()`.
- [ ] **Step 6: `examples/hotkey_probe.rs`** — installs the service, prints events; with `--selftest-start-menu N`
  injects (untagged) the chord as a 400 ms hold N times and checks via `GetForegroundWindow` + process name
  that `StartMenuExperienceHost.exe`/`SearchHost.exe` never became foreground (prints `START_OPENED k/N`);
  with `--passthrough` injects Ctrl+Win+Right then Ctrl+Win+Left and reports Wren produced `Cancel` only.
  Run both with Glaido/Willow not running (check first; if running, report SKIPPED, don't kill them).
- [ ] **Step 7: Commit.**

---

### Task 3: Voice in — audio capture, speech sidecar, engine supervisor

**Files:** Create `src-tauri/src/audio.rs`, `src-tauri/src/engine/{mod.rs, protocol.rs, supervisor.rs}`,
`sidecar/pyproject.toml`, `sidecar/uv.lock`, `sidecar/wren_sidecar/{__init__.py, __main__.py, engine.py,
protocol.py, cuda.py}`, `sidecar/tests/test_protocol.py`, `sidecar/assets/warmup.wav` (10 s Hazel en-GB TTS),
`scripts/setup-runtime.ps1`, `scripts/make-test-wavs.ps1`, `src-tauri/tests/engine_roundtrip.rs`.
Modify `lib.rs` (module lines), `tauri.conf.json` (`bundle.resources`: `../sidecar/wren_sidecar/**`,
`../sidecar/pyproject.toml`, `../sidecar/uv.lock`, `../sidecar/assets/**`).

**Interfaces — Produces:**
```rust
// audio.rs
pub struct Recording { pub wav_path: PathBuf, pub duration_ms: u64, pub heard_sound: bool }
pub struct AudioRecorder;  // new(device: Option<String>, level_tx: crossbeam_channel::Sender<f32>) -> anyhow::Result<Self>
                           // start(&self, wav_path: PathBuf) -> anyhow::Result<()>   (stream starts here)
                           // stop(&self) -> anyhow::Result<Recording>  ;  cancel(&self)
                           // silence_ms(&self) -> u64 (continuous silence so far, for hands-free auto-stop)
                           // elapsed_ms(&self) -> u64 ; set_device(&self, Option<String>) -> anyhow::Result<()>
pub fn list_input_devices() -> Vec<String>;
// engine
pub enum EngineStatus { Starting, WarmingUp, Ready { model: String, device: String, vram_mb: u64 },
                        Failed { reason: String } }   // device "cuda" or "cpu"
pub struct TranscribeRequest { pub id: String, pub audio_path: PathBuf, pub bias_terms: Vec<String>, pub prompt: String }
pub struct Segment { pub start: f32, pub end: f32, pub text: String, pub no_speech_prob: f32 }
pub struct TranscribeResult { pub text: String, pub speech_ms: u64, pub infer_ms: u64, pub segments: Vec<Segment> }
pub enum EngineError { NotReady, Oom(String), BadAudio(String), Internal(String), Timeout }
pub struct Engine;  // start(paths: &Paths, settings: &EngineSettings, status_tx: Sender<EngineStatus>) -> Self
                    // transcribe(&self, req: TranscribeRequest, timeout: Duration) -> Result<TranscribeResult, EngineError>
                    // status(&self) -> EngineStatus ; restart(&self) ; shutdown(&self)
```
Sidecar CLI: `python -m wren_sidecar --models-dir <dir> --model <name> --compute <type> --beam <n>
--cpu-fallback <name> --warmup-wav <path>`; protocol exactly as spec §5.

- [ ] **Step 1:** `sidecar/tests/test_protocol.py` — parse/serialise every message type; `transcribe` on a
  missing file → `error` `BAD_AUDIO`; a silent WAV → `result` with `text == ""` and `speech_ms < 300`.
- [ ] **Step 2:** implement sidecar (`cuda.py` = DLL-directory recipe from
  `C:\Overseer\overseer\voice\overseer_voice\stt.py`, never import Overseer). Model + settings default from
  `docs/research/stt-benchmark.md` (fallback: spec §5). Warm-up before `ready`. CUDA failure → CPU fallback
  model with `device:"cpu"` in `ready`.
- [ ] **Step 3:** `scripts/setup-runtime.ps1` — `uv venv --python 3.12` + `uv sync` into
  `%LOCALAPPDATA%\Wren\runtime`, download models into `%LOCALAPPDATA%\Wren\models` (official HF repos).
  Idempotent. Run it. `scripts/make-test-wavs.ps1` — Hazel (en-GB SAPI) 16 kHz mono WAVs into
  `src-tauri/tests/fixtures/` (sentences listed in the script, incl. one with a made-up name) + a 3 s silent WAV.
- [ ] **Step 4:** `audio.rs` — cpal default/named input, start stream on `start`, resample to 16 kHz mono
  (rubato), write 16-bit WAV (hound), RMS level → perceptual 0..1 at `LEVEL_EVENTS_HZ`, `heard_sound`.
  `WREN_TEST_AUDIO` seam (spec §4). Unit test: seam file in → WAV out with matching duration ±50 ms.
- [ ] **Step 5:** engine supervisor — spawn `runtime_python()` with `CREATE_NO_WINDOW`, assign to a Job Object
  (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), stderr → `logs/sidecar.log`, JSON-lines reader thread, request map
  by id, back-off restarts per constants.
- [ ] **Step 6:** `tests/engine_roundtrip.rs` (ignored unless `WREN_LIVE=1`): start engine, wait ready ≤ 60 s,
  transcribe each fixture, assert expected words present (case-insensitive) and silent WAV → empty;
  report infer_ms p50; kill the sidecar process → `Ready` again within 5 s; drop Engine → no child alive.
- [ ] **Step 7:** run, record numbers in your report. Commit.

---

### Task 4: Store, dictionary data, stats, retention, learning diff

**Files:** Create `src-tauri/src/store/{mod.rs, schema.rs, stats.rs}`, `src-tauri/src/dictionary/{mod.rs,
learn.rs}`, `src-tauri/resources/common-words.txt` (top ~20k English words, one per line, from a
permissively licensed list; note the source in the file header). Modify `lib.rs`, `commands.rs` (new
section), `AppState` (add `store: Store`).

**Interfaces — Produces:**
```rust
pub enum PasteOutcome { Pasted, NotPasted, Copied, Failed }      // stored as text
pub enum Mode { Hold, HandsFree }
pub struct NewTranscription { pub id: String, pub created_at_utc: DateTime<Utc>, pub mode: Mode,
  pub audio_path: Option<PathBuf>, pub duration_ms: u64, pub speech_ms: u64, pub raw_text: String,
  pub tidied_text: String, pub final_text: String, pub model: String, pub device: String,
  pub timings_json: String, pub target_process: String, pub paste_outcome: PasteOutcome,
  pub applied_rule_ids: Vec<i64> }
pub struct TranscriptionRow { id, created_at_utc, mode, duration_ms, final_text, raw_text, words,
  target_process, paste_outcome }                                   // serde camelCase to UI
pub struct Stats { time_saved_ms_all, time_saved_ms_week, wpm_today: Option<f32>, wpm_all: Option<f32>,
  streak_days: u32, busiest_hour: Option<u8>, words_today, words_week, words_all, dictations_all,
  daily: Vec<DayPoint { date: String, words: u64 }>   /* last 30 days, oldest first, zero-filled */,
  hourly: Vec<u64> /* 24 entries, last 30 days */ }
pub struct DictionaryEntry { id: i64, kind: EntryKind(Term|Replacement), from_text: Option<String>,
  to_text: String, source: Source(WillowSeed|Manual|LearnedHistory|LearnedPaste), status: Status(Candidate|
  Active|Disabled), observations: u32, hits: u32, created_at: String }
pub struct Replacement { pub id: i64, pub from: String, pub to: String }   // in dictionary/mod.rs
pub struct Correction { pub from: String, pub to: String }               // in dictionary/learn.rs
pub fn diff_corrections(pasted: &str, edited: &str) -> Vec<Correction>;  // spec §8 rules
pub fn is_common_word(word: &str) -> bool;
pub enum LearnDecision { AddedTerm, AddedReplacement, CandidateObserved, Ignored(String) }
pub struct Store;  // open(path) ; insert_transcription(&NewTranscription) (hourly_totals same tx)
  // set_paste_outcome(id, PasteOutcome) ; recent(limit) ; search(query) ; get(id) ; delete_transcription(id)
  // delete_all_history() ; sweep(now: DateTime<Utc>, audio_dir: &Path) -> SweepReport
  // stats(now_local: DateTime<Local>, typing_wpm: u32) -> Stats
  // dictionary_list() ; add_term(word, Source) ; add_replacement(from, to, Source, Status) ; set_status(id, Status)
  // delete_entry(id) ; record_hits(&[i64]) ; bias_terms(max) -> Vec<String> ; active_replacements() -> Vec<Replacement>
  // learn(from_paste_or_history: Source, transcription_id, Correction) -> LearnDecision   (spec §8 promotion)
  // fix_word(transcription_id, from, to) -> (DictionaryEntry /*replacement*/, Option<DictionaryEntry> /*term*/)
```
Commands (camelCase JSON): `history_recent(limit)`, `history_search(query)`, `history_delete(id)`,
`history_delete_all()`, `stats_get()`, `dictionary_list()`, `dictionary_add_term(word)`,
`dictionary_add_replacement(from, to)`, `dictionary_set_status(id, status)`, `dictionary_delete(id)`,
`history_fix_word(id, from, to)`.

- [ ] **Step 1:** tests: schema migrates from empty; insert updates `hourly_totals` in the same transaction
  (simulate failure → neither written); sweep deletes rows + WAVs older than 72 h and orphan WAVs, keeps
  hourly totals; stats formulas against hand-computed fixtures (spec §9) incl. streak across midnight and a
  BST change, zero-filled 30-day series; search LIKE; dictionary CRUD; bias ranking (manual/learned by hits
  then recency, then seed, cap); `diff_corrections` table (`"stand in the Q"`→`"stand in the queue"` ⇒ Q→queue;
  case-only, punctuation-only, insertion, deletion, 3+ edits ⇒ none; `"Klode is great"`→`"Claude is great"`);
  promotion rules (non-common from-word ⇒ replacement at once; common or ≤2 chars ⇒ candidate until 2nd
  sighting). (No Willow import: his Willow dictionary holds only snippets — see SPEC §8.)
- [ ] **Step 2–4:** fail → implement → pass. **Step 5:** commit.

---

### Task 5: Text pipeline and paste

**Files:** Create `src-tauri/src/pipeline/{mod.rs, guards.rs, tidy.rs, replace.rs}`,
`src-tauri/src/paste/{mod.rs, clipboard.rs}`, `src-tauri/examples/paste_probe.rs`. Modify `lib.rs`.

**Interfaces — Consumes:** `dictionary::Replacement` (Task 4), `hotkey::{chord_keys_physically_up,
send_tagged_keys}` (Task 2). **Produces:**
```rust
pub struct PipelineInput<'a> { pub raw: &'a str, pub speech_ms: u64, pub clip_ms: u64, pub prompt: &'a str,
                               pub replacements: &'a [Replacement] }
pub struct PipelineOutput { pub tidied: String, pub final_text: String, pub applied_rule_ids: Vec<i64>,
                            pub repetition_flagged: bool }
pub fn run(input: &PipelineInput, ai: Option<&dyn Fn(&str) -> Option<String>>) -> PipelineOutput;
pub struct ForegroundInfo { pub hwnd: isize, pub pid: u32, pub process_name: String }
pub fn foreground() -> ForegroundInfo;
pub enum PasteResult { Pasted, FocusChanged, KeysStillHeld, OwnWindow, Failed(String) }
pub fn paste_text(text: &str, target: &ForegroundInfo, is_own: &dyn Fn(isize) -> bool) -> PasteResult;
pub fn copy_text(text: &str) -> anyhow::Result<()>;   // excluded from clipboard history / cloud
```
- [ ] **Step 1:** pipeline table tests for every step in spec §6 (fillers incl. "her", "error", "ER",
  "Uh, I think" → "I think", orphan commas, "i'm" → "I'm", hallucinations on short clips only, repetition
  collapse, replacements whole-word/case-aware/longest-first with applied IDs, AI step rejected when > 30 %
  length change, never empty → empty stays empty, no trailing space/newline).
- [ ] **Step 2–4:** fail → implement → pass.
- [ ] **Step 5:** clipboard exactly as SPEC §7: back up all plain-memory formats, delayed-rendering write from a
  hidden owner window (`WM_RENDERFORMAT` = read receipt, 10 ms polls up to 1000 ms), exclusion formats,
  Ctrl+Shift+V retry, restore `CLIPBOARD_RESTORE_MS` after the read only if the sequence number is unchanged,
  `OpenClipboard` retry 5 × 20 ms. Handy's `src-tauri/src/clipboard.rs` and `paste_tx/windows.rs` are useful
  MIT references. Test (desktop): put an image on the clipboard, `paste_text` into the test
  page (`scripts/test-page.html`, opened with `msedge --app=file:///...`), then the image is back
  (hash compare) and Win+V history does not contain Wren's text (best effort: check
  `Windows.ApplicationModel.DataTransfer.Clipboard.GetHistoryItemsAsync` via PowerShell if available, else
  note "not verified").
- [ ] **Step 6:** `examples/paste_probe.rs` — opens/finds the test page window, focuses it, pastes three
  strings, reads them back via UI Automation (or the page writes them to a file via `localStorage` dump —
  simplest: the page title echoes the textarea value) and reports pass/fail. Commit.

---

### Task 6: Design system and popup UI

**Files:** `src/styles/{tokens.css, base.css}`, `src/components/{Card.tsx, StatTile.tsx, Button.tsx,
Toggle.tsx, Sidebar.tsx, Icon.tsx, Toast.tsx}`, `src/popup/{Popup.tsx, popup.css, LevelBars.tsx}`,
`src/lib/{api.ts, types.ts, mock.ts}`, `src/App.tsx` (shell with sidebar + Home page),
`src/pages/Home.tsx`, `scripts/screens.mjs` (Playwright, `channel: "msedge"`), `src-tauri/icons/*`
(Wren icon set from one 1024 px source via `npx tauri icon`).

**Interfaces — Produces:**
```ts
// types.ts — mirrors Rust (Task 4/8). PopupState is what Task 8 emits on "popup://state".
export type PopupState =
  | { kind: "hidden" } | { kind: "listening"; cpu: boolean } | { kind: "handsfree"; startedAtMs: number; cpu: boolean }
  | { kind: "warming" } | { kind: "transcribing"; sinceMs: number } | { kind: "pasted" } | { kind: "nothing" }
  | { kind: "noSound" } | { kind: "notPasted"; text: string } | { kind: "learned"; from: string; to: string; entryId: number }
  | { kind: "error"; message: string } | { kind: "capWarning"; remainingMs: number };
// api.ts — typed wrappers over invoke() for every command in Tasks 1, 4, 8, 9; falls back to mock.ts when
// window.__TAURI_INTERNALS__ is absent (Playwright screenshots).
```
Popup click actions call Task 8 commands: `popup_copy_text(text)`, `popup_undo_learned(entryId)`,
`popup_discard()`, `popup_open_main()`.
- [ ] Build tokens from `docs/research/willow-teardown.md` §8 (light theme, finish) and the layout/sizing from
  `docs/research/glaido-teardown.md` (Main window layout spec, Popup spec). Improve on both: spec §11 numbers.
- [ ] Popup: every state in spec §11, Glaido-sized pill, spring-like grow/shrink, level bars from real level
  events (smoothed, ~60 fps), hands-free timer, `prefers-reduced-motion`. Transparent page background.
- [ ] Home: four stat tiles + 30-day strip + recent cards; click card → copy (`navigator.clipboard.writeText`
  inside the main window) with "Copied" feedback; hover icons Fix a word / Delete (Fix opens an inline
  word-picker: click a word, type the replacement, Save → `history_fix_word`).
- [ ] `scripts/screens.mjs`: builds with Vite, serves `dist`, screenshots Home (empty + populated mock) and
  every popup state at 1100×700 / popup size into `docs/screenshots/`. Look at each screenshot yourself and
  fix anything that looks off before finishing.
- [ ] Commit.

---

### Task 7: Main window pages

**Files:** `src/pages/{History.tsx, Dictionary.tsx, Stats.tsx, Settings.tsx}`, `src/components/*` additions,
`scripts/screens.mjs` (add pages).
**Consumes:** Task 6 components/api; Task 4 commands; Task 8/9 commands for engine status, mic devices,
watcher and AI clean-up status (`engine_status()`, `audio_devices()`, `watcher_status()`,
`ai_cleanup_status()`, `ai_cleanup_pull_model()`, `other_apps_running()`, `engine_restart()`).
- [ ] History (search, grouped by day, cards with copy/fix/delete, "Deleted after 3 days" note), Dictionary
  (Words / Replacements / Learned tabs, add forms, undo for learned entries), Stats (totals, 30-day
  chart, hour-of-day chart with busiest hour highlighted, WPM), Settings (spec §11 list). Empty states for
  every list. Startup banner when `other_apps_running()` returns Willow/Glaido.
- [ ] Screenshots of every page (empty + populated mock) via `scripts/screens.mjs`; inspect and fix. Commit.

---

### Task 8: Session — wire it all together, hands-free, self-test, live test

**Files:** Create `src-tauri/src/session.rs`, `src-tauri/src/selftest.rs`, `scripts/test-page.html`,
`scripts/live-test.ps1`. Modify `lib.rs` (start services in `setup`, `--selftest` arg), `commands.rs`.
**Consumes:** Tasks 2–5. **Produces:** Rust `PopupState` enum (serde `tag = "kind"`, camelCase) matching
Task 6's TS type; commands `popup_copy_text`, `popup_undo_learned`, `popup_discard`, `popup_open_main`,
`engine_status`, `engine_restart`, `audio_devices`, `set_audio_device`, `other_apps_running`; app events
`app://engine-status`, `app://history-changed`.
- [ ] Session thread owns: dictation IDs, state, ordered result queue. Flow per spec §3–§7: HoldStart →
  `audio.start` + `POPUP_DELAY_MS` popup → HoldEnd → stop, `Transcribing`, engine (bias terms, prompt) →
  pipeline (replacements; AI step if enabled) → `store.insert` → `paste_text` → `store.set_paste_outcome` →
  popup `Pasted`/`notPasted`; Tap/Cancel → discard (or save if ≥ `CANCEL_SAVE_AFTER_MS`); hands-free start/
  stop, cap warning + stop, silence auto-stop, lock/sleep stop; warming-up path; pause from tray; reinstall
  hook from tray; history-changed event after each insert; engine status forwarded.
- [ ] `--selftest`: hook installs, mic present (or seam), engine ready, DB writable, bundled WAV → expected
  text; prints `SELFTEST PASS`/`FAIL <reason>` and exits.
- [ ] `scripts/live-test.ps1`: refuses if session locked or Willow/Glaido running (prints SKIPPED). Starts
  Wren with `WREN_HOME` temp + `WREN_TEST_AUDIO`, opens the test page, focuses it, injects a 3 s chord hold,
  checks the page received the expected text, checks the foreground window was the test page throughout,
  50× Start-menu check, passthrough check (Ctrl+Win+Right), image-clipboard survival, hands-free double-tap
  run. Writes `docs/live-test-report.md`.
- [ ] Commit.

---

### Task 9: Correction watcher, Claude probe, AI clean-up

**Files:** `src-tauri/src/watcher/{mod.rs, uia.rs}`, `src-tauri/examples/uia_probe.rs`,
`src-tauri/src/ai_cleanup.rs`; modify `session.rs` (start watcher after paste; AI step), `commands.rs`
(`watcher_status`, `ai_cleanup_status`, `ai_cleanup_pull_model`).
- [ ] Watcher per spec §8 on its own thread (COM MTA), TextPattern then ValuePattern, slow-call guard,
  stops on focus change / empty box / next dictation / 60 s; feeds `diff_corrections` → `store.learn` → popup
  `learned` toast. Test against `scripts/test-page.html` (textarea + contenteditable): paste, edit a word with
  injected keys, assert a learned entry appears.
- [ ] `uia_probe`: **read-only** against the running Claude desktop app (`claude.exe`): find its main window,
  locate the composer/editable element, read its text 20× and time it; write
  `docs/research/claude-uia-probe.md` (what is readable, p50/p95 ms, verdict). Never send input to Claude.
  Set the default of `watcher_enabled` from the verdict.
- [ ] AI clean-up via Ollama (spec §10), off by default; status command; pull button runs `ollama pull`.
  Test with the switch on against a short sentence (pull `qwen2.5:3b` if absent). Commit.

---

### Task 10: Package, install, verify end to end

- [ ] `npm run tauri build` (NSIS, per-user). Install silently (`/S`) to the per-user location. Run
  `Wren.exe --selftest` from the installed copy. Run `scripts/live-test.ps1` against the installed exe.
  Screenshots of the real running app (main window via PowerShell window capture; popup states via the
  live test). `README.md` (what it is, how to run, where data lives, how to rebuild). Commit.

### Task 11: UX polish pass (separate reviewer, then fixes)

- [ ] A fresh reviewer looks only at experience: every screenshot, popup animation timings, load time of the
  main window, empty states, copy, alignment, light-theme contrast, the feel of the level bars. Findings to
  `docs/reviews/ux-polish.md`; an implementer fixes them; screenshots refreshed.
