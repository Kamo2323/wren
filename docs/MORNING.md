# Good morning, Maximilian — Wren is built, installed and running

**Try it now (30 seconds):** click into a Claude message box → hold **Left Ctrl + Left Win** → say a sentence →
let go. Your words appear about a third of a second later. Wren is already running in the tray (bottom-right).

## How to use it

- **Hold to talk:** hold Left Ctrl + Left Win, speak, let go → pasted where your cursor is.
- **Hands-free:** double-tap Left Ctrl + Left Win → the popup shows a timer → tap the keys once to finish (or
  click × to throw it away). It stops itself after 10 minutes, or after 90 s of silence (saved, not pasted).
- **A paste went wrong?** Open Wren (tray icon or Start menu → Wren) → Home → click the dictation → it's copied.
- **It misheard a word?** Just fix the word in the box straight after it pastes. Wren notices and shows
  "Learned X → Y · Undo"; next time it writes it right. You can also fix a word from History.
- **Your words never leave this PC.** Speech-to-text runs on your RTX 3060 Ti (Whisper large-v3-turbo, ~1.3 GB of
  graphics memory). No accounts, no limits, free forever.

## Before you rely on it (2 minutes)

1. **Glaido is closed** (I closed it for the hotkey tests — not uninstalled, settings untouched). Willow wasn't
   running. If either starts again, both apps react to Ctrl+Win and you get double pastes — Wren shows a warning
   banner when it sees them.
2. Both probably start with Windows: Task Manager → Startup apps → disable Willow Voice and Glaido. Then in Wren:
   Settings → **Start with Windows** on (I left it off so a reboot can't start three dictation apps at once).

## What only you can check (I verified everything else — numbers below)

- [ ] Your real voice in Claude: is the British-accent accuracy good? (The model was picked by testing 22 setups on
      200 of your old Willow recordings.)
- [ ] Hold Ctrl+Win with your real keyboard a few times: the Start menu should never open.
- [ ] The first word of a sentence isn't cut off (automated tests feed audio from files, so they can't see this).
- [ ] The popup's feel (size like Glaido's, smoother animation).
- [ ] Fix a word in Claude right after a paste → you should see the "Learned" toast.
- [ ] Win+Shift+S (screenshot) and Win+V still work while Wren runs (automation can't open those safely).

## What was verified overnight

- Live end-to-end test on the **installed** app: 10/10 holds pasted, hands-free pasted, chat-style box pasted;
  release → text on screen **p50 317 ms / p95 326 ms**; the popup never stole focus (12/12); the Start menu never
  opened (0/50); Ctrl+Win+→ still switches desktops; a screenshot on your clipboard survives a dictation; Wren never
  adds to your Win+V history.
- Correction learning live: "drizzly" → "drizzle" learned 0.17 s after editing, in a plain box and a chat composer.
- Claude's message box can be read instantly (< 1 ms), so correction learning is **on** by default.
- Installed self-test: hotkey, mic, database, GPU engine — PASS.
- ~230 automated Rust tests + sidecar Python tests + front-end checks, all green.

## Things that happened overnight you should know about

- **Your Win+V clipboard history lost about 12 older (unpinned) entries.** An early paste test added 12 entries,
  pushing older ones out; I removed the test entries but the old ones can't be recovered. Fixed at the root — Wren
  never adds clipboard-history entries now.
- A hotkey test opened Notepad, which reopened your `FILM5-DESIGN.md` tab (nothing typed; file unchanged).
- At 03:07 (Prague) someone pressed real keys ("g", "l") on the PC while Glaido was being studied — I assume you.
- The Glaido study sent 5 silent test recordings to Glaido's servers (nothing pasted, nothing in your Glaido
  history list).
- Two empty "Pick an application" windows opened during tests; I closed them.
- Downloads: Whisper models (2.0 GB, `%LOCALAPPDATA%\Wren\models`), the speech engine runtime (2.3 GB,
  `%LOCALAPPDATA%\Wren\runtime`), the Ollama model `qwen2.5:3b` (1.9 GB, for the optional AI clean-up), and the
  open-source Handy app's code (MIT) as a reference.

## Decisions I made for you (change any of them)

- Name: **Wren**. Built with Tauri (like Glaido — light and fast) + a local Python speech engine.
- Hotkey timings copied from Willow (which you're used to): a hold over 0.4 s = talk; two taps within 0.6 s =
  hands-free.
- "Time saved" uses a 52 words-per-minute typing speed (Glaido's figure) — change it in Settings.
- Your Willow dictionary only held 4 text shortcuts (which you said you don't want), so nothing was imported.
- AI clean-up (a small local model that tidies punctuation) is **off** by default — it adds ~0.1–0.3 s.
- "Fix a word" also corrects that dictation in History.
- Wren refuses to paste when the focused thing is clearly not a text box (it copies instead and tells you), so a
  dictation can't vanish.

## Known limits / not done

<!-- filled in at the end of the night -->

## Where everything is

- App: `%LOCALAPPDATA%\Programs\Wren\wren.exe` (Start menu → Wren). Data: `%LOCALAPPDATA%\Wren`, settings:
  `%APPDATA%\Wren`. Uninstalling never touches your data.
- Code: `C:\Wren` (git). Start with `README.md`; the design is `docs/design/SPEC.md`; the full decision log is
  `docs/HANDOFF.md`.
- Rebuild + reinstall after a change: `powershell -ExecutionPolicy Bypass -File scripts\release.ps1`.
