# Meeting transcripts: who said what, on calls with many people — brief for a fresh chat

Status: **not started — a starting point, NOT a fixed plan.** Draft PR from branch
`feat/transcript-speaker-accuracy` (off `main` at 99beee0).

> **Maximilian's instruction:** this plan isn't set in stone. Before building anything, brainstorm it with
> him using the **`/egm:egm-brainstorm`** skill. Treat everything below as input to that brainstorm, not
> decisions. Only build what comes out of it.

## Why

The main thing about Meetings is that the transcript is on point: right words, right person. Wren has only
ever been tested on one-person Teams calls. Maximilian can't easily get five people on a call, so we need a way
to test and tune speaker naming without them.

## How speaker naming works today (verified in code, 2026-11-26)

- **Me** lines come from the microphone, so they're always "Me".
- **Them** is one mixed system-audio stream: Wren can't tell voices apart from sound.
- Names come from **Teams' own "who's speaking" highlight**, read through Accessibility on macOS
  (`src-tauri/src/meetings/teams/ax_macos.rs`) and UI Automation on Windows (`teams/uia.rs`), parsed in
  `teams/parse.rs` (`SPEAKING_STATES = ["active speaker", "speaking"]`, `" is speaking"` suffix).
- `src-tauri/src/meetings/speakers.rs`: each Them line takes the name Teams highlighted most during it, shifted
  by `TEAMS_LAG_MS = 900`. Below `MIN_CONFIDENCE = 0.35` → "Speaker"; below `LOW_CONFIDENCE = 0.6` → name greyed
  with "?". The user's own Teams name is never given to a Them line. **Rename all** fixes names by hand.
- Participant names from Teams are already fed to the transcriber as bias terms (`meetings/session.rs`
  `process`: `terms.extend(timeline.participants())`), so names get spelled right.
- **Unconfirmed:** the wording Teams uses when *someone else* is speaking has never been seen live. If it differs,
  every other person shows as "Speaker" until a parser tweak (ledger: Task 14 ruling).

## Ideas to brainstorm (Claude's proposals, 2026-11-26)

**Testing without five people**
1. **Solo multi-device test call.** Join one Teams meeting from several devices under different names: Mac
   (Wren, "you"), Windows PC Teams ("Tom"), iPhone Teams ("Sarah"), 1–2 browser tabs joined as guests via the
   meeting link. Read a script, unmuting one device at a time. Include fast back-and-forth and one overlap.
   Attribution uses Teams' highlight, not voices, so one human voice is fine. Compare the transcript with the
   script → an accuracy score.
2. **"Save call diagnostics" switch.** During that call, save the Teams readings (speaking / roster / mute over
   time) next to the meeting's audio, so we can confirm Teams' wording and replay the call.
3. **Replay benchmark.** Turn the recorded call + script into a test: same audio and Teams readings, with the
   right answers attached. Every change gets scored ("92% of lines named correctly").

**Improvements (in proposed order)**
1. **Split a line when the speaker changes.** Today quick back-and-forth inside one clip goes to whoever spoke
   longest. Cut the line where Teams' highlight moves. Medium effort, biggest win on big calls.
2. **Voice fingerprints as a backup.** A small on-device speaker model groups Them lines by voice; Teams'
   highlight names the groups. It fills gaps where Teams shows nobody (crosstalk, screen-share layouts that hide
   tiles, minimised window). Bigger job; only if the test shows those gaps are common.
3. **Auto-tune Teams' delay** per call instead of a fixed 0.9 s. Small.

**Open questions for the brainstorm**
- Which devices can join the test call (iPhone Teams? browser guests allowed by his tenant?)
- Mac first (most meetings are there), Windows after, or both at once?
- How good is "good enough" (a target accuracy)?
- Is word accuracy (not just names) a problem today too? (Engine: Whisper large-v3-turbo; MLX on Mac, CUDA on
  Windows.)
- Privacy: diagnostics contain names; keep them local and auto-delete like audio (24 h)?

## Useful context
- `docs/HANDOFF.md` (top) and `.superpowers/sdd/2026-11-24-meetings/progress.md` (local ledger, git-ignored).
- `scripts/teams-call-probe.ps1` (Windows accessibility dump during a call). The Mac has no equivalent script yet.
- Live testing on the MacBook goes through a T3 thread on "Lord's MacBook Pro" (`/Users/maximilian/Code/Wren`).
