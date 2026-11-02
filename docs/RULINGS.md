# Wren — every ruling made during the overnight build (2026-11-02)

Decisions the coordinator took on Maximilian's behalf, in order, each with what it costs if wrong. Source: the build ledger.

Ruling 1: Task 1 implemented by the coordinator, not a subagent — it is scaffolding and fixes the shapes every
Ruling 2: Implementers run in PARALLEL in separate git worktrees (skill says sequential) on disjoint modules;
Ruling 3: `scripts/test-page.html` is created by Task 5 (it needs it for its paste test); Tasks 8 and 9 extend
Ruling 4: Willow import dropped — his Willow dictionary.json holds only 4 text shortcuts (snippets, which he
Ruling 5: Hotkey timings follow Willow (tap < 400 ms, double-tap gap 600 ms) instead of the Goldfish 250/350 —
Ruling 6: Time-saved typing baseline 52 wpm (Glaido's; matches numbers he has seen) — cost if wrong: one setting.
Ruling 7: Clipboard paste uses delayed rendering as a read receipt (Willow's technique) with Ctrl+Shift+V retry —
Ruling 8: Glaido closed by the coordinator (not uninstalled, settings untouched) so live hotkey tests do not
Ruling 9: Reviewers run at effort xhigh, implementers at high, all on Opus (his standing rules).
Ruling 11: harness `isolation: worktree` creates worktrees of the session's repo (C:\Overseer), not C:\Wren; builders made their own at C:\Wren-wt\task-N. From now on the coordinator creates C:\Wren-wt\task-N before dispatch and passes the path; no `isolation` flag — cost if wrong: none (Overseer untouched, verified with git status).
Ruling 12: "Speed" tile label (unit wpm) accepted over "Words per minute" — fits at 960 px — cost if wrong: one label.
Ruling 13: hands-free pill ~148 px (timer + ×) accepted over spec ~134 — cost if wrong: CSS tweak.
Ruling 14: PopupState startedAtMs/sinceMs are epoch ms; capWarning gains mode — Task 8 must send these.
Task 4: open notes: WPM/time-saved use duration_ms (spec ambiguous; Goldfish defined speaking = chord down to stop) — Ruling 15: keep duration_ms — matches Glaido's "minutes spoken" feel — cost if wrong: one column swap. T8 must call record_hits for terms found in final text.
Ruling 16: engine = turbo int8_float16 beam 1, vad_filter on — benchmark winner, lowest VRAM — cost if wrong: one settings change.
Task 2: fix round 1/5 (9 addressed, 0 open; commit 59e543a) — coordinator diff read of the Important (on_session_interrupted → Cancel for hands-free, test renamed + asserts Cancel{9000}); hook.rs restructure (+241: install-last, priority, retries) accepted on live evidence (8/8 modes, START_OPENED 0/50 re-run) — Ruling 17: no separate re-review seat for the hook.rs minors; Task 8 review and the final review cover hotkey integration — cost if wrong: a hook-lifecycle bug found later.
Benchmark: complete (93d608c on main). Ruling 18: bias via initial_prompt 'Glossary: …' only, no hotwords (benchmark: hotwords appended terms to silence; prompt raised recall 80→93%) — sent to Task 3 mid-run; SPEC §5 updated — cost if wrong: one sidecar flag.
Ruling 19: restart-after-kill target relaxed to ≤ 8 s + back-off — model load (3.4 s) + warm-up (1 s) + 1 s back-off cannot fit 5 s; a crash is rare — cost if wrong: a slower recovery after a sidecar crash.
Ruling 20: anything that must land in the real %LOCALAPPDATA% (runtime, models, the NSIS install, the morning launch) is started via WMI Win32_Process.Create, never as a child of the Claude desktop app (MSIX redirects AppData writes) — cost if wrong: files invisible to him.
Ruling 21: fix_word also rewrites the transcription's final_text in Rust — "fix a word" must stick in History and copy — cost if wrong: a changed record he didn't expect (it is his own correction).
Ruling 22: add speakingMs to DayPoint and draw a real 30-day WPM trend — he loves stats dashboards, data already in hourly_totals — cost if wrong: small schema-free stats change.
Ruling 23: remove "mm" from the filler list ("10 mm" must survive); remove "err" only in filler position (sentence start or comma-adjacent) — his dictation includes code/measurements — cost if wrong: an occasional "mm" left in text.
Ruling 24: Task 9 adds a UIA pre-paste check (≤ 50 ms budget): if the focused element is definitely not editable, don't paste — copy + notPasted; if UIA is slow/unavailable, paste anyway — prevents dictations vanishing into History when the composer isn't focused — cost if wrong: a false "not pasted" in an exotic editor.
Ruling 25: engine failure keeps the WAV, retries once, then saves a 'failed' row with the audio — a long hands-free dictation must never be lost — cost if wrong: an empty History row for a failed clip.
Ruling 26: no separate re-review seat for the Task 8 concurrency fix — mutation-tested + live-verified; the final whole-branch review covers session.rs/popup.rs concurrency — cost if wrong: a late concurrency finding.
Ruling 27: install dir must not be %LOCALAPPDATA%\Wren (data root) — uninstall could delete db/models/runtime — cost if wrong: data loss on uninstall.
Ruling 28: leave the installed Wren running in the tray (launched outside the sandbox) for the morning; Start-with-Windows stays off — cost if wrong: he sees an extra tray icon.
Ruling 29: no separate re-review seat for Task 9 r1 — pure decide()/track/guard changes with RED→GREEN tables; live re-run after reinstall + final review cover it — cost if wrong: a late finding.
Ruling 30: Task 10 has no separate task-review seat — scripts/installer verified by a real install + uninstall + live run on the installed exe; the final whole-branch review covers selftest.rs/lib.rs changes — cost if wrong: a late finding.
