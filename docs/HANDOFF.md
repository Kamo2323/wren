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

## Progress (updated ~05:35 UTC 2026-11-02)

- Research done: Goldfish review, Willow + Glaido teardowns, STT benchmark (`docs/research/stt-benchmark.md`:
  large-v3-turbo int8 beam 1 + VAD + Glossary prompt).
- Merged on main: Task 1 foundation, Task 2 hotkey, Task 3 voice in (audio + sidecar + engine), Task 4 store/stats,
  Task 5 pipeline + paste (review running), Task 6 design system + popup + Home.
- In progress: Task 7 pages (fix round), Task 8 session wiring (worktree C:\Wren-wt	ask-8-session).
- Remaining: Task 9 watcher + Claude probe + AI clean-up, Task 10 installer + live verification, Task 11 UX polish,
  final review, morning note.
- Every ruling and review outcome: `.superpowers/sdd/PLAN/progress.md` (git-ignored ledger).
- Speech runtime + models installed for real at `%LOCALAPPDATA%\Wren\{runtime,models}` (via WMI, because processes
  started from the Claude desktop app get AppData writes redirected into Claude's package cache).

## Things to tell Maximilian in the morning

- Glaido was closed (not uninstalled) for the hotkey tests and left closed so Wren doesn't double-paste.
- Task 5's first paste tests added 12 entries to his Win+V clipboard history, pushing ~12 older unpinned entries
  out (unrecoverable). Root-fixed; Wren never adds history entries now.
- A Glaido debug relaunch at 02:07 UTC saw real (non-injected) "g", "l" key presses — someone was at the PC.
- Task 2's live probe opened Notepad, which reopened his FILM5-DESIGN.md tab (unchanged).
- wordfreq's word list (used to judge "common words") is CC BY-SA — fine for a personal app.

## Next step

Finish Tasks 5/7 reviews, merge Task 8, then dispatch Task 9, Task 10, Task 11, final review, morning note.
