# Wren — concept input for the Goldfish brainstorm review

Date: 2026-11-02. Owner: Maximilian (single user, personal app, not for sale or distribution).

## What it is

Wren is a free Windows desktop dictation app. You hold a hotkey, talk, let go, and the words you said are
typed into whatever text box has focus. It replaces two paid/limited apps the owner uses today: Willow Voice
(installed, Electron) and Glaido (installed, Tauri). Glaido is the owner's preferred reference for features
and layout; Willow is the preferred reference for the light colour theme and visual finish.

## Who uses it and where

- One person, on one PC: Windows 11 Home, AMD Ryzen 5 5600X, 16 GB RAM, NVIDIA RTX 3060 Ti with 8 GB VRAM
  (about 3.4 GB of VRAM already in use by other software at idle).
- 98% of dictation goes into the Claude desktop app (an Electron app). The rest goes into the Codex desktop
  app (also Electron).
- English only, spoken with a British accent. Mis-heard words are the owner's biggest complaint about
  existing apps.
- The owner dictates a lot, so per-word or per-minute cloud limits are not acceptable.

## Hard constraints

1. Completely free to run. Speech-to-text runs locally on the owner's GPU. No paid API. No reuse of Willow's
   or Glaido's servers, keys or accounts.
2. One hotkey only: Left Ctrl + Left Win.
   - Hold it: record while held, transcribe and paste on release.
   - Double-tap it: start hands-free recording; double-tap (or single tap) again to stop and paste.
   - Releasing the Win key must never open the Start menu.
   - No other global hotkeys at all (Willow's Win+Shift mode broke the owner's Win+Shift+S screenshots).
3. Its own standalone app. Nothing to do with the owner's other project (Overseer), even though that project
   also runs faster-whisper on the same GPU.
4. Fast. The owner will turn off anything that makes dictation noticeably slower.

## Features in scope

- Hold-to-talk and double-tap hands-free on the one hotkey (above).
- A small recording popup near the bottom-centre of the screen while recording. The owner likes the size of
  Glaido's popup; its animation is good but should be improved.
- Paste into the focused app at the cursor.
- Always-on instant tidy-up with no model call: strip filler words ("um", "uh", "er"), fix stray lowercase
  "i", clean doubled spaces.
- Optional AI clean-up with a local language model, behind a switch that is OFF by default.
- Custom dictionary of words and names, used to bias the speech model and to correct its output.
- Learning from corrections: after Wren pastes text, if the owner edits a mis-heard word in the text box
  (for example changes "Q" to "queue"), Wren notices and adds the correction to the dictionary so the word
  comes out right next time. Must work in the Claude desktop app if technically possible. Fallback: correct
  the word in Wren's history screen and Wren learns from that.
- History of every transcription, searchable. The text and any audio are deleted after 3 days.
- Stats dashboard: time saved, words per minute, day streak, busiest hour, a chart of daily use. Daily
  totals (counts only, no text) are kept forever so the stats survive the 3-day text deletion.
- Home page: big stats at the top, then recent transcriptions as large cards. Clicking a card copies its
  text to the clipboard (recovery for a paste that failed).
- Main window look: Glaido's layout (big statistics, big recent activity, very little clutter) combined with
  Willow's light theme and finish. Big, minimal, simple, modern, light.
- Starting dictionary seeded from the owner's existing Willow dictionary file.

## Out of scope (decided by the owner)

- Agent commands, custom tools, web search, any "do things for me" voice mode.
- Snippets / text shortcuts.
- Different styles per app.
- Other languages, other operating systems, mobile, accounts, sync, teams, billing, auto-update servers.
- "Longest dictation" stat.

## Proposed technical direction (for the reviewers to challenge)

- Desktop shell: Tauri 2 (Rust core + web front end in the system WebView2), the same family as Glaido.
  Rust owns the low-level keyboard hook (SetWindowsHookEx WH_KEYBOARD_LL on its own thread), Start-menu
  suppression (inject a harmless masking key before Win is released), paste (save clipboard, set text,
  SendInput Ctrl+V, restore clipboard), the popup window, the local database (SQLite) and the UI.
- Speech: a Python helper process ("sidecar") using faster-whisper or NVIDIA Parakeet on CUDA, started by the
  app and kept warm with the model loaded. The model is chosen by benchmarking candidates against ~1,100 of
  the owner's own past Willow recordings (with Willow's transcripts as the reference).
- Correction learning: read the focused text box through Windows UI Automation after a paste, watch it for
  a short window, diff against what was pasted, and turn single-word substitutions into dictionary entries.
- Front end: React + TypeScript + Vite.

## Known risks the owner has heard about

- Local AI clean-up costs ~1–2 s and ~3 GB VRAM; that is why it is off by default.
- Electron apps (Claude desktop) may not expose their text to UI Automation reliably.
- Two dictation apps cannot share one hotkey; Willow and Glaido must be closed while Wren runs.
