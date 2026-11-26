# Mac: stop the Keychain pop-up after every update (Claude key in a private file)

**Status:** brief only, nothing built in this PR. Decision made by Maximilian on 2026-11-26: **option A**.

## The problem

On the Mac, after every Wren update, macOS shows *"Wren wants to access key "Wren" in your keychain. To allow
this, enter the "login" keychain password"*. "Always Allow" doesn't stick across updates.

## Why it happens (evidence from the MacBook, 2026-11-26)

- `securityd` logged exactly two prompts in 3 days, each on the **first key read after an update**
  (0.1.12 at 23:41 on 25 Nov, 0.1.13 at 11:43 on 26 Nov). Relaunches of the same build read the key with no
  prompt (0.1.12 wrote enhanced notes at 00:12Z without one).
- Up to 0.1.11 the updater re-signed each update locally with Maximilian's *Apple Development* identity
  (Team ID 3GCJRS9FR2), so the Keychain partition was `teamid:…` and stayed the same.
- Since 0.1.12 (`bd21d91`, "keep privacy permissions across updates") releases keep Wren's **self-signed**
  certificate (`TeamIdentifier=not set`). That keeps Microphone / Accessibility / Screen Recording
  permissions across updates (they follow the designated requirement), but the legacy Keychain's
  partition list remembers an app without a team as `cdhash:<this build>`. Every release has a new CDHash,
  so every update asks again.
- Wren also reads the key from the OS store on **every** use (`KeyStore::system().get()` in
  `ai_cleanup/mod.rs`, `meetings/enhance.rs`, `meetings/commands.rs`), so a *Deny* would re-prompt on every
  dictation.

## Options considered

| | Option | Trade-off |
|---|---|---|
| **A (chosen)** | On macOS keep the key in a private file in Wren's folder (owner-only, mode 0600), moved out of the Keychain once | No prompts for anyone, free. Other programs running as the same user could read the file without asking (same as `~/.aws/credentials`, most developer tools). |
| B | Apple Developer Program ($99/yr), Developer ID + notarization (already supported: `WREN_SIGN_MACOS=true`) | The proper long-term fix, also removes Gatekeeper warnings; needs Maximilian to enrol. Could replace A later. |
| C | Keep the Keychain, read once per launch | Still one password prompt per update. |

## What to build (A)

1. **Key file** `~/Library/Application Support/Wren/claude-api-key` (the `Paths::resolve().roaming` folder),
   created owner-only (0600), written to a temp file then renamed into place.
2. **One-time move:** on the first read, if there's no key file, read the Keychain entry (service `Wren`,
   account `anthropic-api-key`). That's one last prompt. Then write the file, delete the Keychain copy, and
   drop a marker file (`claude-api-key.moved-from-keychain`) so the Keychain is never read again.
   - Keychain has no key → write the marker, no prompt next time.
   - Prompt denied / Keychain locked → no marker, nothing deleted, try again next launch (the key isn't lost).
   - File write fails → keep the Keychain copy, use the key, try the move again next launch.
3. **Save / Remove key** on Mac write or delete the file and set the marker, so an old Keychain copy can never
   come back. If the move hadn't happened yet, also try to delete the Keychain copy (the user asked for it).
4. **Read once per launch** (both platforms): cache the key in memory, updated by Save/Remove, held under a
   lock so two callers can't both trigger a prompt. On Mac a failed Keychain read is cached too (no prompt per
   dictation after *Deny*). On Windows a failed Credential Manager read isn't cached (retried next use).
5. **Windows unchanged** apart from the cache: Credential Manager never prompts.
6. **Settings text** (Mac): "The key is kept in a private file in Wren's folder that only your Mac account can
   open, never in Wren's settings or logs." Today it says "the macOS Keychain".
7. Nothing in Wren copies or exports the settings folder (checked: only `paths.rs` uses `.roaming`), so the
   file never leaves the Mac. The key stays out of logs.

## Tests

- Unit tests (any platform, temp folder):
  - The key is moved once and the Keychain is never read again.
  - "No key" is checked only once.
  - A denied read is retried and nothing is deleted.
  - A key saved before the move wins.
  - Remove never brings back the Keychain copy.
  - Overwriting works, and an empty file means no key.
  - On unix, the file is mode 0600.
- Probe `examples/key_move_probe.rs`:
  - It creates a throwaway OS-store entry (service `Wren-key-move-probe`), moves it into a temp key file, and checks the entry is gone and the file is 0600.
  - It never touches Wren's real key. The same binary creates the entry, so the Mac doesn't prompt.
- Mac `cargo test --lib`, then a release build installed over 0.1.13. **Maximilian clicks Allow once.** After that, update or relaunch again and check there's no prompt and clean-up and enhanced notes still work.

## Needs Maximilian

- Click **Allow** one last time after the update that ships this (the move). Then confirm a later update doesn't ask.
- Optional, later: whether to join the Apple Developer Program (option B). It would also let the key go back into the Keychain.

## Note

A working draft of A (tests passing on Windows: 500 passed, 0 failed) exists on the **local, unpushed** branch
`fix/mac-claude-key-file` in `C:\Wren-keyfile`. It was written before Maximilian said he only wanted the PR;
use it or ignore it when this gets built.
