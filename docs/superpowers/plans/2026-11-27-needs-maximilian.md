# Next Wren update — the parts that need Maximilian

Status: **open.** Everything that could be built and proven without Maximilian went into the combined
update PR (branch `update/wren-four-prs`, which merges PRs #1, #4, #5 and #6), built overnight
26–27 Nov 2026. This brief lists what's left: checks only he can run, decisions that are his, and the
work that depends on them. Start here in a new session.

## 1. Checks only Maximilian can run (about 30 minutes in total)

| # | Check | Why it needs him | How |
|---|---|---|---|
| 1 | **Speaker test call on the Mac** | Needs several devices on one Teams call | Follow `docs/meetings-speaker-test-call.md`: Save call diagnostics on, Mac + Windows Teams + iPhone Teams + a browser guest, read the script. Then `cargo run --example speaker_replay` on the saved file. **First thing to look at:** do the `speaking` lists in `call-diagnostics.jsonl` show the *other* people while they talk? That settles the open question of how Teams words "someone else is speaking". |
| 2 | **Email button on Windows** | Needs Outlook signed in to his account | Open a meeting → Enhanced → **Email**. Does new Outlook open the draft as an editable new email (good) or as a read-only message? If read-only, see decision 3 below. |
| 3 | **Clean-up on the Mac, in the installed app** | Needs his real Mac apps, Keychain key and mic | After updating: Settings → Tidy text with Claude → turn it on (it starts **off** after the update, see decision log). Dictate into Claude, T3 Code, Terminal, Mail, Messages and Gmail in Chrome. Check each is shaped for that app, and try "slash compact" in Claude. |
| 4 | **Copy for email** on Windows | Needs his Outlook/Gmail | Copy → paste into Outlook (classic and new) and Gmail in Edge. Formatting kept? |
| 5 | *(optional)* Mute gap, live | Needs a real Teams call | On a call, mute, say something private, unmute. The private bit must not be in the notes. Unit-tested on both platforms; a live check is extra confidence. |

## 2. Decisions that are his

1. **Speaker accuracy, what to build next** (the brainstorm the brief asked for; run `/egm:egm-brainstorm`
   with him). Open questions from `docs/superpowers/plans/2026-11-26-transcript-speaker-accuracy.md`:
   - Which devices can join the test call (iPhone Teams? browser guests allowed by his tenant?)
   - Mac first, or both at once?
   - A target accuracy ("good enough" = ? % of lines named right on the 5-person benchmark; today 93.3%
     on the synthetic fixture).
   - Is word accuracy (not just names) a problem today too?
   - Diagnostics privacy: kept local and deleted with the audio after 24 h — OK as is?
2. **Line splitting** (the biggest speaker win): it needs the Mac transcriber's timestamps fixed first
   (the Mac cuts silences out before transcribing, so chunk times drift) and chunk timings passed through
   to Wren. Worth doing now, or after the test call shows how often lines mix two people?
3. **If new Outlook opens .eml read-only:** switch Email to (a) Outlook's own "new mail" via COM for
   classic Outlook, (b) a `mailto:` link (plain text only, no formatting), or (c) keep .eml and say
   "Copy" is the best route for new Outlook.
4. **Clean-up spend:** is the "This month" line enough, or does he want a monthly cap / warning?

## 3. Work that waits on the above

- Speaker naming improvements after the brainstorm: line splitting, voice fingerprints as a backup,
  auto-tuning Teams' delay (fixed 0.9 s today), parser changes if Teams words other speakers differently,
  and a Mac version of `scripts/teams-call-probe.ps1`.
- Whatever the Email check (1.2) decides.

## 4. Small engineering follow-ups (don't need him; out of scope for the update)

- `cargo clippy --all-targets` fails on macOS: four Windows-only examples (`hotkey_probe`, `live_test`,
  `paste_probe`, `uia_probe`) aren't marked Windows-only. Pre-existing.
- `meetings/enhance_tests.rs` leaves `wren-enh-<uuid>` folders in `%TEMP%` (551 on this PC). Pre-existing.
- Notes tables: header shading stops at the last column instead of the card edge. Pre-existing.
- Multi-line clean-up output pasted into a bare shell prompt: low risk (zsh/Terminal hold a pasted block
  until Enter; Windows Terminal warns on multi-line paste; PSReadLine inserts without running), but a
  "one line for plain terminals" option could be added if it ever bites.
- PDF on a Letter-region PC wasn't run live (A4 was). The "no mail app" message is unit-tested only.
- `npm run screens` wasn't re-run after the clean-up card changed.
- An empty folder `C:\Wren-live.lock` was left at the root of C: by an overnight builder; the agent
  safety guard blocks deleting paths directly under C:\, so delete it by hand.

Created with Claude Opus 5.5 in Claude Code (T3 Code).
