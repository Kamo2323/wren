# Combined update (`update/wren-four-prs`) — live check + UX polish, Windows PC, 2026-11-26

Test sub-agent run on one debug build of the combined branch (base 07f5bc3, `WREN_BUILD_ID=fourprs-1126`,
rebuilt as `fourprs-1126-polish` after the polish commits). Temp `WREN_HOME`, real speech runtime
(`WREN_RUNTIME_ROOT=%LOCALAPPDATA%\Wren`, large-v3-turbo on CUDA), Windows TTS WAVs as `WREN_TEST_AUDIO`
(the file is reopened per recording, so it was swapped between dictations), the real Ctrl+Win chord injected
only while the target window was in front, Playwright over CDP for the page. Installed Wren was stopped for the
run and restarted afterwards; `%TEMP%\wren-live.lock` taken and released.

## Phase 1 — live proof

### Claude clean-up (real key from Credential Manager, Haiku 5.5)
| Target window (process) | Category | Whisper → pasted | Claude ms | release→paste ms |
|---|---|---|---|---|
| Notepad `general.txt - Notepad` | general | "So the quarterly report is due on Friday and I still need the figures from Sam. Can you remind me tomorrow?" → same text | 510 | 887 |
| Chrome app window `Paste bug - Claude` | coding (Rewrite) | "Okay so um can you, can you look at the settings page? It is, it is not saving the theme when I restart the app, and then run the unit tests and tell me what fails." → "Can you look at the settings page? It isn't saving the theme when I restart the app. Then run the unit tests and tell me what fails." | 450 | 945 |
| same | coding | "slash compact" → `/compact` | 382 (cache read 940 tok) | 659 |
| `Inbox (2) - me@example.com - Gmail` | email | "Hi Sam thanks for sending the notes over. I will read them tonight … Best wishes, Max." → "Hi Sam,\n\nThanks for sending the notes over. I'll read them tonight and get back to you tomorrow.\n\nBest wishes,\nMax" | 477 | 889 |
| `WhatsApp` | messaging | "Yeah that works for me. I will be there at 2. Can you grab the projector?" → "Yeah that works for me. I'll be there at 2. Can you grab the projector?" | 439 | 801 |
| `Paste bug - Claude`, style switched to **Tidy** in the real Settings card (saved `"codingStyle":"tidy"`) | coding (Tidy) | same coding WAV → "Okay so can you, can you look at the settings page? It is, it is not saving the theme when I restart the app, and then run the unit tests and tell me what fails." (words kept, "um" dropped) | 500 | 964 |

No key (`WREN_KEYSTORE_MEMORY=1`, empty in-memory store; the real saved key untouched): the card shows the
switch disabled + "Add your Claude key under Meeting notes to turn this on." + "Go to Meeting notes" (clicking it
focuses `#claude-key`). A Notepad dictation pasted Whisper's text unchanged, `pipeline_ms 0`, no Claude request.
(A first try was "not pasted": the test harness's Alt tap had opened Notepad's menu and Wren's pre-paste check
correctly saw a `MenuItem`. Harness fixed, retried.)

### Notes export (seeded UUID meeting + medium note, Enhanced tab, real buttons)
- PDF → native Save dialog → toast "Saved notes-….pdf"; file `%PDF-`, 46,233 bytes, 1 page 595.0×841.9 pt (A4),
  text contains Kraków / Résumé / 李雷 / Owner / Weekly sync; `%TEMP%\Wren\pdf` empty afterwards. Repeated on
  the rebuilt (polish) app: same result.
- Email → toast "Opening a new email in Outlook"; `.eml` 6,500 bytes, Subject "Weekly sync with Sarah — Q3
  “plan” & Zoë", `X-Unsent: 1`, MIME 1.0, no To, text/plain + text/html utf-8 base64, ASCII-only, 0 bare LF,
  0 parser defects. new Outlook (`olk.exe`) launched; closed again by the test (it needs a sign-in).

### Call diagnostics
- Switch turned on in the real Settings page → `"saveCallDiagnostics": true` in settings.json → still on after
  restarting the app.
- Test meeting ("Start meeting notes", Teams only, Teams not running, 37 s): `call-diagnostics.jsonl` created
  next to me.wav/them.wav at start: 1 header + 150 `teams` readings (~4/s), every `speaking`/`roster` empty,
  0 `line` records (no Teams audio, so no transcript lines). `speaker_replay extract` parses it (`"lines": []`).
  Real Teams readings were not available here.

### Replay benchmark — `cargo run --example speaker_replay -- fixtures`
- five-person: **14 of 15 (93.3%)**, 0 wrong, 1 unnamed (#12, Teams showed nobody), 4 mixed.
- one-person: **6 of 6 (100%)**.

### Screenshots (real app via CDP, not the mock page)
`docs/screenshots/claude-cleanup/real-app-card-{light,dark,960-light}.png`, `real-app-card-no-key-{light,dark}.png`;
`docs/screenshots/speaker-accuracy/real-app-meeting-notes-{light,dark,960-light}.png`;
`docs/screenshots/windows-notes-export/real-app-enhanced-{1200,960}-{light,dark}.png`.

## Phase 2 — UX polish (commits 876b425, 8024e37)
1. **Long toasts ran off the window.** `.toast` was `white-space: nowrap`; the "No email app…" advice at 960 px
   started 64 px left of the window edge, and vanished after 2.4 s. Now toasts wrap at
   `min(460px, 100vw - 40px)` (short ones unchanged: "Saved ….pdf" is one line, 38 px), and export error toasts
   stay 7 s (as Fix permissions does). `ux-polish/long-toast-before-960-dark.png` → `long-toast-after-960-{dark,light}.png`,
   `short-toast-pdf-saved.png`.
2. **"Settings → Apps → Default apps"** read like Wren's own Settings → now "Choose one in Windows Settings → Apps →
   Default apps (for .eml files), or use Copy and paste the notes into an email."
3. **Spend line** for a clean-up-only month read "~$0.00 across 0 notes · <$0.01 across 6 clean-ups" → now
   "<$0.01 across 6 clean-ups (estimated from list prices)." `ux-polish/spend-line-{before,after}-960-dark.png`.
4. **Diagnostics help** reworded ("During each call, Wren saves what Teams shows (who is speaking, everyone in the
   call by name, whether you're muted) and the transcript lines to a file next to the meeting's audio…").

Looked at and left as is: the clean-up card (copy, spacing, model dropdown, Rewrite/Tidy segments, no-key state)
in both themes at 960 and 1200; the Enhanced button row (one line at 1200; at 960 it wraps under the footer label,
left-aligned, all five buttons on one line).

After the changes: `npm run build` ok; `node scripts/unit-tests.mjs` 38 passed, 0 failed;
`node scripts/check-pages.mjs` 170 passed, "all checks passed"; `cargo test --lib` 491 passed, 0 failed, 16 ignored.

## Not proven / below standard
- Diagnostics with real Teams readings (needs a Teams call; the guide is `docs/meetings-speaker-test-call.md`).
- new Outlook opening the `.eml` as an editable draft (still needs a signed-in Outlook).
- Pre-existing, not from this branch: the notes table's header shading stops at the last column instead of the
  card edge (`.md table { display: block; width: 100% }` in meetings.css). Left alone.
- `%TEMP%` holds hundreds of `wren-enh-<uuid>` folders from earlier runs/tests (not investigated).
