# Speaker test call: checking who-said-what without five people

A solo Teams call, joined from several devices under different names, that gives Wren a real
multi-person call to name. Wren saves what Teams showed (**call diagnostics**), you write down who
really said each line, and the **replay benchmark** scores Wren: "13 of 15 lines named correctly".
Every later change to speaker naming is scored against the same recording.

Wren names Them lines from Teams' "who's speaking" highlight, not from voices, so one human voice
reading every part is fine.

## What you need

| Device | Joins as | Notes |
|---|---|---|
| Mac with Wren | you (your normal account) | Wren records here. Teams sound on **headphones** (Wren still hears Teams; the other devices' mics don't). |
| Windows PC, Teams app | **Tom** | Second Microsoft/Teams account, or join the meeting link as a guest named "Tom". |
| iPhone, Teams app | **Sarah** | Third account or guest. |
| Browser tab(s) (Edge/Chrome) on the PC or another laptop | **Priya**, **Alex** | "Join on the web instead", type the name. Needs guest access to be allowed for your meetings; if it isn't, skip them and run a 3-person call. |

- Turn the **speaker volume to zero** on every device except the Mac (no echo loops).
- **Mute every device** at the start. You unmute only the device whose line you're reading.
  Shortcuts: Teams on Windows / web `Ctrl+Shift+M` (toggle) or hold `Ctrl+Space` (hold to talk);
  iPhone: tap the mic.
- The Mac's own mic will hear you read the other parts. That's fine: those "Me" lines are outside
  the score (and show as muted if the Mac is muted in Teams).

## 1. Switch diagnostics on (Mac)

Wren → **Settings → Meeting notes → Save call diagnostics** → on. It applies from the **next**
meeting you start. While on, each meeting writes `call-diagnostics.jsonl` next to its audio. It holds
the Teams names and the transcript lines, stays on the Mac, and is deleted with the audio
**24 hours** after the meeting ends, or when you delete the meeting. Switch it off after the test.

## 2. Run the call

1. Start the Teams meeting on the Mac and admit Tom, Sarah, Priya and Alex.
2. In Wren, start meeting notes. Wait for the participant names to appear.
3. Read the script below. Unmute the named device, read, mute it again, then go on.
   - **Normal turns**: leave about a second between speakers.
   - **Fast back-and-forth** (marked ⚡): switch as fast as you can, aiming for under ¾ s of
     silence. This is where one transcript line ends up holding two people.
   - **Overlap** (marked ⧉): unmute *both* named devices and read the line once, so Teams lights both.
   - **Teams shows nobody** (marked ▢): share your screen from the Mac first (Teams hides the tiles in
     some share layouts), or minimise the Teams window on the Mac, then read the line.
4. Stop meeting notes in Wren. Leave Teams.

### Script

| # | Device | Line |
|---|---|---|
| 1 | Tom | Right, let's start. Everyone can see the agenda? |
| 2 | Mac (you) | Yes, I can see it. Can everyone hear me? |
| 3 | Sarah | Yes, and I added the supplier item at the bottom. |
| 4 | Priya | I can take the supplier item, I spoke to them on Monday. |
| 5 | Alex | Can I go first? I have to drop at half past. |
| 6 ⚡ | Tom | Sure, go. |
| 7 ⚡ | Sarah | Wait, which slide? |
| 8 ⚡ | Tom | The second one, the one with the dates on it. |
| 9 ⚡ | Sarah | That one's old. |
| 10 ⚡ | Priya | No, I updated it last night, it's the March version now. |
| 11 ⚡ | Alex | Okay so. |
| 12 ⚡ | Tom | Go on Alex. |
| 13 ⚡ | Alex | So the numbers are up. |
| 14 | Sarah | By how much? |
| 15 | Tom | Twelve percent. |
| 16 | Sarah | That's brilliant. |
| 17 ⧉ | Priya + Tom | And the supplier confirmed the delivery date as well, yes they did, last week. |
| 18 ▢ | Alex | I'm sharing my screen now, you should see the dashboard with the weekly figures. |
| 19 | Sarah | Got it, the blue line is this year? |
| 20 | Mac (you) | Thanks, that's clear. |
| 21 | Tom | Yes, blue is this year and grey is last year. |
| 22 | Priya | Lovely. I'll send the supplier contract round today. |

(Stop sharing before line 19.) The synthetic fixture `five-person` in
`src-tauri/tests/fixtures/speaker-replay/` is this same script, so you can compare.

## 3. Copy the diagnostics out (within 24 hours)

The meeting folder is the newest folder in:

- Mac: `~/Library/Application Support/Wren/meetings/`
- Windows: `%LOCALAPPDATA%\Wren\meetings\`

```sh
# Mac
M=$(ls -td ~/Library/Application\ Support/Wren/meetings/*/ | head -1)
mkdir -p ~/speaker-test && cp "$M/call-diagnostics.jsonl" ~/speaker-test/
```

**First check (the big unknown):** open the file and look at the `"speaking"` lists in the `teams`
lines while Tom, Sarah and the guests were talking. If they are `[]` the whole time, Teams words
"someone else is speaking" differently from what Wren expects, every other person comes out as
"Speaker", and the parser needs a tweak before any scoring is worth doing. Send the file over.

## 4. Write the answer key

From the Wren repo (`src-tauri` folder):

```sh
cargo run --example speaker_replay -- extract ~/speaker-test/call-diagnostics.jsonl > ~/speaker-test/case.json
```

`case.json` now lists every Them line with its times and text, `"expected": null`, and the names
Teams showed (`participants`). For each line, set `expected` to who really said it, **spelled
exactly as in `participants`** (case doesn't matter). If a line holds more than one person (the ⚡
and ⧉ lines often do), put whoever said most of it in `expected` and the others in
`"alsoSpeaking": ["…"]`. Leave `null` for lines you can't place (they aren't scored).

## 5. Score it

```sh
cargo run --example speaker_replay -- score ~/speaker-test/call-diagnostics.jsonl ~/speaker-test/case.json
# try other Teams delays (default 900 ms):
cargo run --example speaker_replay -- score ~/speaker-test/call-diagnostics.jsonl ~/speaker-test/case.json --lag 600
```

Output (from the synthetic five-person fixture):

```text
Case: five-person (Teams lag 900 ms)
  #      audio (s)  expected             named                 conf  result   Teams showed
  5      22.0-27.5  Tom Reid             Tom Reid              0.66  ok       - > Tom Reid > - > Sarah Jones > - > Tom Reid  [mixed]
  7      34.5-38.0  Alex (Guest)         Alex (Guest) ?        0.54  ok       - > Alex (Guest) > - > Tom Reid > - > Alex (Guest)  [mixed]
 12      51.0-56.0  Alex (Guest)         Speaker               0.00  unnamed  -
Score: 14 of 15 lines named correctly, 93.3% (0 wrong name, 1 unnamed; 4 mixed lines; 0 not scored)
```

- **named**: what Wren would call the line; `?` = shown greyed (confidence under 0.6);
  `Speaker` = no name (under 0.35).
- **result**: `ok`, `WRONG` (another person's name: the worst case), `unnamed`, or `-` (not scored).
- **Teams showed**: who Teams highlighted during the line (shifted by the delay), in order; `-` = nobody.
- **[mixed]**: more than one person in one line. It can score `ok` while the other person's words are
  still credited to the main speaker; the count of mixed lines is what "split a line when the speaker
  changes" would fix.

`cargo run --example speaker_replay -- fixtures` scores the bundled synthetic fixtures;
`cargo test --lib replay` checks them.

## File formats

- `call-diagnostics.jsonl`: see the module docs in `src-tauri/src/meetings/diagnostics.rs`.
- `case.json`: see `src-tauri/src/meetings/replay.rs`.
- The synthetic fixtures are generated by `node scripts/make-speaker-fixtures.mjs`.

## What to send back

The score output, `case.json` and `call-diagnostics.jsonl` (the names in it are the test names, but
it also holds the transcript of the call), and anything odd you noticed: a device Teams didn't
highlight, guests not allowed, the Mac minimised, and so on.
