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

## Build result (2026-11-26)

The brainstorm with Maximilian couldn't happen (unattended build), so the lead scoped this PR to the
parts that change nothing he hasn't approved: tools to measure speaker naming, not changes to it.
Speaker naming behaviour (`speakers.rs`) is unchanged.

### Built
1. **"Save call diagnostics" switch** — Settings → Meeting notes, **off by default**, read when a meeting
   starts. While on, the meeting writes `call-diagnostics.jsonl` next to `me.wav`/`them.wav`: every Teams
   reading (speaking highlight, roster, own name, mute state, title) and every saved line (times, name,
   confidence, text), all on the meeting's audio clock. Deleted by the 24 h audio sweep (`delete_wavs` now
   also removes it) and with the meeting folder on delete. Help text says it holds names and stays local.
   Code: `src-tauri/src/meetings/diagnostics.rs` (format documented there), wired in `session.rs`.
2. **Replay benchmark** — `src-tauri/src/meetings/replay.rs` replays the readings through the real
   `SpeakerTimeline::record` + `attribute` (same calls as the meeting worker) and scores Them lines against
   an answer key (`case.json`, format in the module docs). CLI: `cargo run --example speaker_replay --
   fixtures | score <diag> <case> [--lag MS] | extract <diag>`. Synthetic fixtures in
   `src-tauri/tests/fixtures/speaker-replay/` (generated by `scripts/make-speaker-fixtures.mjs`):
   `one-person` → 6 of 6 (100%); `five-person` (fast back-and-forth, one overlap, a stretch where Teams
   shows nobody) → 14 of 15 (93.3%), 1 unnamed, 4 mixed lines. Scores are pinned in tests.
3. **Test-call guide** — `docs/meetings-speaker-test-call.md`: devices, script (same as the five-person
   fixture), switching diagnostics on, copying the file out, writing the answer key, scoring.

### Not built: splitting a Them line when the speaker changes
Investigated the transcriber output first. It gives Whisper **segment** timestamps (no word timestamps),
and they aren't usable yet:
- **Mac (the main platform):** `sidecar/wren_sidecar/mlx_backend.py` runs Silero VAD and transcribes only
  the concatenated speech (`_run_silero_vad` → `collect_chunks`), so segment times are on the
  silence-removed audio and drift from the clip's real times after the first pause. They'd need mapping
  back (faster-whisper does this itself on Windows; the MLX path doesn't).
- The `Transcriber` trait returns text only; `EngineTranscriber` drops `segments`, and the hallucination
  guards (`pipeline::guards::apply`) can change the text after the segments were made, so pieces would
  need re-aligning.
- Whisper segments end at sentence/pause boundaries, which only roughly match turn changes.
So the split would be built on timings that are wrong on the Mac, and couldn't be scored on real audio
here. The benchmark already counts "mixed" lines (4 of 15 in the five-person fixture), which is the
number a split would bring down.

### Decisions
- Diagnostics are JSON Lines (one write per record) so a crash loses at most one line; every reading is
  kept (dropping repeats would change replay: attribution caps each reading at 1 s). Roster/name/title are
  only written when they change, to keep big calls small (~3 MB/hour at 4 reads/s).
- Lines (with text) are included so the answer key can be built from the file alone; the help text says so.
- The setting is read at meeting start ("Starts with the next meeting").
- A mixed line counts as correct when it gets the majority speaker; it's flagged `[mixed]` and counted.
- Expected names are compared to Teams' names ignoring case and a "(You)" suffix, no fuzzy matching.

### Proof
- `cargo test --lib`: 459 passed, 0 failed, 16 ignored. New: diagnostics round trip + bad input,
  off = no file, on = readings + lines on the audio clock (and replay names the line as the live meeting
  did), deleted on meeting delete, deleted by the 24 h sweep (kept before), setting off by default,
  replay fixture scores, lag sensitivity, replay == live `attribute`, skeleton extraction.
- `cargo clippy --all-targets`: no warnings in new or changed lines (older warnings elsewhere remain).
- `npm run build` ok; `node scripts/unit-tests.mjs` all passed; `node scripts/check-pages.mjs` all passed
  (3 new checks for the switch).
- Screenshots: `docs/screenshots/speaker-accuracy/` (light off, dark on). Taken from the mock-mode page,
  **not the real app**: the shared `C:\Wren-live.lock` was held by another builder for over two hours, so
  the live debug-app run was skipped.

### Left for Maximilian (follow-up PR)
- Run the test call (guide above) on the Mac; first check the file's `speaking` lists show other people
  (confirms Teams' wording for someone else speaking — the open ledger question).
- Line splitting: map MLX VAD timestamps back to clip time, pass segments through `Transcriber`, then
  split at highlight changes (respect `TEAMS_LAG_MS`, minimum piece length) and score with the benchmark.
- Voice fingerprints, auto-tuning the Teams lag (the benchmark's `--lag` shows sensitivity), parser changes
  for other-speaker wording, a Mac equivalent of `scripts/teams-call-probe.ps1`.
