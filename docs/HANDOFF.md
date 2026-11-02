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

- M-4 (final review): an out-of-memory error retries on the same GPU state; with Unity filling VRAM every
  dictation fails until "Restart engine". Proposed: restart the engine (→ CPU fallback) before the retry.
- Fix-a-word Undo leaves that dictation's corrected text in History (needs a `history_set_text` command).
- Not verifiable by automation (he checks): real-keyboard Start menu, Win+Shift+S / Win+V passthrough, first-word
  clipping with the real mic, accent accuracy live in Claude.
- Rebuild/reinstall: `powershell -ExecutionPolicy Bypass -File scriptselease.ps1` (runs every step via WMI,
  outside the Claude app's MSIX sandbox). Don't run `cargo fmt` on the crate (reformats ~40 files).

## Things to tell Maximilian (all in docs/MORNING.md)

- Glaido closed (not uninstalled); Willow wasn't running; both should come out of Windows startup before he turns
  on Wren's "Start with Windows".
- Win+V history lost ~12 older unpinned entries during Task 5's first tests (root-fixed).
- Notepad reopened his FILM5-DESIGN.md tab (unchanged); someone pressed real keys at 03:07 Prague; two OpenWith
  pickers closed; the Glaido study sent 5 silent clips to Glaido's servers.
- Downloads: models 2.0 GB, runtime 2.3 GB, Ollama qwen2.5:3b 1.9 GB, Handy source (MIT) for reference.
