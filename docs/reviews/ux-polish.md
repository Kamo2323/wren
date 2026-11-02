# Wren — UX polish review

Scope: look, feel and copy only (functional review is done elsewhere). Read-only review of the real
installed app's screenshots (`docs/screenshots/real/`), the design-preview shots (`docs/screenshots/`),
Glaido's real UI (`docs/research/glaido/`, teardown §3–§4), Willow's theme notes (teardown §7–§8) and
the source in `src/` and `src-tauri/`. The app was not run.

Note: `docs/screenshots/real/` has 8 of the 12 shots the brief lists. There are no real captures of
stats, settings, settings-bottom or popup-transcribing, so those pages were judged from the preview
shots only.

## Verdict

**Main window: it beats Glaido and is level with Willow on finish, apart from one thing that ruins the
first impression.** The light canvas with a white panel, 44 px figures, purple-tinted shadows, big
copyable cards and a real stats page are cleaner and more "dashboard" than either app. But the
installed app shows a **black Windows title bar** above the light UI (real `home.png`, `history.png`,
`dictionary.png`). Willow and Glaido both hide theirs, so this is the first thing he will see and it
will look worse than both. Second biggest problem: every page switch replays a full entrance
animation, up to about 1.25 s on Home and Stats, so moving around feels slower than Glaido, which
switches instantly.

**Popup: Glaido's size matched exactly, motion better, a few rough edges.** The hold pill is 116 × 38,
the same as Glaido. Hands-free is about 150 px (Glaido's is 134). The 11 rippling level bars, the
spring and the shimmer are calmer and livelier than Glaido's sine wobble and hard cut, and Wren has
notices that Glaido doesn't (learned, not pasted, no sound). Where it falls short:

- **The "pasted" tick never finishes drawing before the pill leaves.** He sees this moment on every
  dictation.
- **The pill floats 26 px above the taskbar,** over the bottom of the app he is typing into, where the
  text box usually sits. Glaido's sits on the taskbar edge.
- **The whole 360 × 120 popup window probably catches clicks** while it is clickable (likely, but not
  confirmed).

Counts: **3 Must fix · 9 Should fix · 8 Nice to have** (20 items).

---

## Findings

### 1. Black native title bar on a light app — **Must fix**
- **Where:** `src-tauri/tauri.conf.json:14-22` (main window has no `theme`), `src-tauri/src/lib.rs:155-158`.
  Seen in the real `home.png`, `history.png` and `dictionary.png`.
- **What's wrong:** Windows is in dark mode, so the caption bar is black (#000-ish) with white text,
  sitting on top of the #f3f3f5 sidebar and white panel. It is the first thing he sees, and it looks
  worse than both apps (Willow and Glaido hide their title bars).
- **Change:**
  1. In `tauri.conf.json` on the `main` window, add `"theme": "Light"` and `"backgroundColor": "#f3f3f5"`.
     The second one also stops the white flash before the first paint.
  2. In `lib.rs`, before `main.show()`, colour the caption to match the canvas so it blends in like
     Willow's hidden bar:
     - Add the `"Win32_Graphics_Dwm"` feature to the `windows` crate in `src-tauri/Cargo.toml:43`.
     - Call `DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR /*35*/, &COLORREF(0x00F5F3F3), 4)`.
     - Call `DWMWA_BORDER_COLOR /*34*/` with the same value.
     - Call `DWMWA_TEXT_COLOR /*36*/` with `COLORREF(0x002B2324)` (that is #24232b).
     - Ignore the error on Windows 10, where step 1 alone still gives a light bar.
  3. Check with a real screenshot: the strip above the sidebar should read as one surface with it.

### 2. The "pasted" tick is cut off and the exit squashes it — **Must fix**
- **Where:** `src/popup/popup.css:347-352` (`.tick path`), `:85-95` (`.pill__content` `content-in`
  50 ms delay), `src/popup/Popup.tsx:33-34, 373-374`. `POPUP_PASTED_MS = 400` is in
  `src-tauri/src/constants.rs:155`.
- **What's wrong:**
  - The tick starts drawing at 90 ms and finishes at 390 ms. The pill starts leaving at 400 ms.
  - The size spring takes about 440 ms to settle.
  - So the success moment is never seen whole: the tick is still drawing while the circle is still
    shrinking, and then it fades out.
  - The exit then squashes the 38 × 38 circle into a **40 × 10** capsule (wider than the circle),
    which reads as a smear rather than a clean "done".
- **Change:** keep 400 ms, but make the animation fit inside it:
  - `.tick path { animation: draw 180ms cubic-bezier(0.65,0,0.35,1) 40ms forwards; }`
  - Add `.pill[data-kind="pasted"] .pill__content { animation-duration: 180ms; animation-delay: 0ms; }`
  - In `Popup.tsx`, collapse a pasted pill evenly: when `display.kind === "pasted"`, use a collapsed
    size of `22 × 22` instead of `40 × 10`.
  - Result: the tick is complete by about 220 ms and holds for about 180 ms before the pill shrinks to
    a point.
  - Also check one capture: real `popup-pasted.png` shows a faint grey tick outside the circle, to the
    lower right. Confirm it is the desktop behind and not a stale WebView2 frame (if it moves with the
    pill, it is a stale frame).

### 3. Every page switch replays the entrance animation, and Home jumps while it loads — **Must fix**
- **Where:**
  - `src/App.tsx:96-103`: `Fragment key={page}` remounts the page.
  - `src/styles/components.css`: `.stat` 376-395, `.strip` 452-457, `.strip__bar` 493-494, `.tcard` 551.
  - `src/styles/pages.css`: `.chart__bar` 800-801, `.total` 565, `.setgroup` 975.
  - `src/pages/Home.tsx:19, 86-87, 116`.
- **What's wrong:**
  - **Replay:** each visit to Home fades the page, staggers the four tiles over 540 ms, raises the
    strip, then grows 30 bars one after another until about **1.25 s**. Stats does the same with its
    bars (160 ms + 29 × 14 ms + 700 ms ≈ 1.27 s). The first time it is lovely; every time after, the
    app feels slower than Glaido, which switches instantly.
  - **Blank placeholder:** every remount also re-fetches with no cache, so the tiles show as a blank
    150 px block for a moment.
  - **Jump:** the "Recent" heading appears first at y≈190, then jumps down about 124 px when the
    30-day strip (only rendered once stats arrive) pushes it.
- **Change:**
  1. **Play the choreography once per app session.**
     - In `App.tsx`, add a `settled` flag that is set 1.4 s after first mount, and put
       `className={`shell ${settled ? "is-settled" : ""}`}` on the shell.
     - In CSS:
       `.shell.is-settled :is(.stat, .strip, .strip__bar, .chart__bar, .total, .setgroup, .tcard) { animation: none; }`
     - Shorten `.page`'s `page-in` to 160 ms.
     - Keep a rise-in only for cards that are genuinely new: give a card `className="tcard is-new"`
       when its id is above the highest id the page had seen before, and animate only `.tcard.is-new`.
  2. **Cache the last data** in module scope in `Home.tsx`, `Stats.tsx` and `History.tsx`, for example
     `let lastStats: Stats | null = null; let lastRows: TranscriptionRow[] | null = null;`. Use it as
     the initial state and refresh in the background. Pages then paint complete on every revisit.
  3. **Reserve the strip on the first load:** when `stats` is null, also render
     `<div className="strip" style={{ height: 110 }} aria-hidden="true" />`. The "Recent" heading then
     never moves.

### 4. The popup sits higher than Glaido's, over the app being typed into — **Should fix**
- **Where:** `src-tauri/src/popup.rs:150-152` (uses `rcWork.bottom`), `src-tauri/src/constants.rs:167`
  (`POPUP_BOTTOM_MARGIN = 12`), `src/popup/popup.css:37` (`padding-bottom: 14px`).
- **What's wrong:**
  - The pill's bottom edge is 26 px above the taskbar, so the pill covers the band 26–64 px above it,
    in the centre. That is where Claude's and the browser's message box sit when the window is
    maximised.
  - The "Learned … Undo" and "Not pasted" notices appear in the same spot, on top of the text he has
    just pasted and may be correcting.
  - Glaido sits on the taskbar's top edge (pill bottom 33 px above the monitor bottom, overlapping the
    taskbar by 15 px). That is the placement he is used to and likes.
- **Change:**
  - Position against the monitor rather than the work area:
    `let y = info.rcMonitor.bottom - h - (19.0 * scale).round() as i32;`
    and set `POPUP_BOTTOM_MARGIN = 19.0`. The pill's bottom is then 33 px above the monitor's bottom
    edge, matching Glaido.
  - If the taskbar is not at the bottom, `rcWork.bottom == rcMonitor.bottom` and this still works.
  - Verify once that the pill draws above the taskbar. If it doesn't, re-assert `HWND_TOPMOST` (no
    activate) on every show, as Glaido does.

### 5. The clickable popup window probably catches clicks around the pill — **Should fix (verify first)**
- **Where:** `src-tauri/src/popup.rs:175-185` (`set_clickable` clears `WS_EX_TRANSPARENT` for the whole
  window). It is clickable in the hands-free, cap-warning, learned (6 s), not-pasted (4 s), no-sound and
  error states.
- **What's wrong:** transparent WebView2 pixels still receive clicks once the window isn't
  click-through. So while hands-free is on (which can last minutes) or a notice is showing, an invisible
  360 × 120 box at the bottom centre swallows clicks meant for the app underneath. To him, that will
  feel like "my click didn't work".
- **Verify:** during hands-free, click 20 px above the pill. If the app underneath gets the click, drop
  this item.
- **Change:** keep the window click-through, and only make it clickable while the cursor is over the
  pill:
  - The popup reports the pill's (and hint's) rectangle with a new `popup_hit_rect` command whenever
    `width` changes.
  - While a clickable state is showing, a 30 Hz timer in Rust compares `GetCursorPos` with that
    rectangle plus 4 px and toggles `WS_EX_TRANSPARENT` accordingly.

### 6. Home tile captions all wrap to two lines at 960 px — **Should fix**
- **Where:** `src/pages/Home.tsx:31, 42-45, 55, 64`; `src/styles/components.css:430-439`
  (`.stat__caption`). Seen in `home-min-960x640.png`.
- **What's wrong:** at the minimum width each tile has about 124 px for text. Every caption wraps
  ("2.7× faster than / typing", "Most words, last / 30 days"), so the row looks cramped and ragged.
- **Change (copy fits at 960 and reads better at 1200):**
  - Time saved: `2 h 18 min this week` → `2h 18m this week`. Add a `formatSavedShort` that gives `2h 18m`,
    `45m` and `1h`.
  - Speed: `2.7× faster than typing` → `2.7× typing speed`. When there is no data:
    `Words ÷ minutes spoken` → `Shows after your first dictation`.
  - Day streak: `Keep it going tomorrow` → `Keep it going`. When there is none:
    `Dictate today to start one` → `Start one today`.
  - Busiest hour: `Most words, last 30 days` → `Last 30 days`.
  - Add `text-wrap: balance;` to `.stat__caption` so any wrap that is left is even.

### 7. At 960 × 640, Home shows only one recent card — **Should fix**
- **Where:** `src/styles/components.css` `.stat` (`min-height: 150px`), `.strip`, `.strip__bars`
  (`height: 44px`), `.recent` (`margin-top: 36px`). Seen in `home-min-960x640.png`.
- **What's wrong:** the tiles, the strip and the gaps take about 430 of the 640 px. He asked for big
  recent activity under the stats, and at the minimum size there is barely one card.
- **Change:**
  ```css
  @media (max-height: 720px) {
    .page--home .stat { min-height: 128px; }
    .page--home .stat__value { padding-top: 12px; }
    .page--home .strip { padding-top: 12px; padding-bottom: 12px; }
    .page--home .strip__bars { height: 28px; margin-top: 8px; }
    .page--home .recent { margin-top: 24px; }
  }
  ```
  That frees about 70 px, so one and a half cards show above the fold.

### 8. Speaking-speed card shows two competing big numbers, and both headline captions break badly — **Should fix**
- **Where:** `src/pages/Stats.tsx:206-213` (the "Today 149" fact), `:151` and `:193` (caption copy);
  `src/styles/pages.css:737` (`.headline__caption`). Seen in `stats-populated-full.png`.
- **What's wrong:**
  - "141 wpm on average" (the main figure) and a bold "Today 149" in the corner fight for attention.
    149 is already in the Today tile at the top of the page.
  - Both captions wrap so that a single word ends up alone on the second line ("…of the last 30 /
    days", "…than the week / before").
- **Change:**
  - Delete the `Today` fact block from `SpeedCard`.
  - Busiest-hour caption → `Your busiest hour · 2,240 words (13%)`, i.e.
    `` `Your busiest hour · ${plural(...)} (${share}%)` ``.
  - Trend copy → `` `${n}% ${delta > 0 ? "up" : "down"} on the week before` `` and
    `Steady for two weeks`. The full caption then reads `2.7× faster than typing · 3% up on the week before`.
  - Add `text-wrap: pretty;` to `.headline__caption`.

### 9. Switched-off toggles are nearly invisible — **Should fix**
- **Where:** `src/styles/components.css:280-309`. Seen in `settings-full.png` (Keep the microphone
  open, Sounds, Start with Windows, AI clean-up).
- **What's wrong:** the off track (#d6d5de) on white has a contrast of about 1.45:1. The WCAG minimum
  for controls is 3:1, so the off switches read as disabled or missing.
- **Change (the Windows 11 pattern, which passes):**
  ```css
  .toggle { background: var(--panel); box-shadow: inset 0 0 0 1.5px #8f8d9b; }
  .toggle__knob { top: 5px; left: 5px; width: 14px; height: 14px; background: #5f5d68; box-shadow: none;
    transition: transform 260ms cubic-bezier(0.34,1.45,0.64,1), background-color 180ms var(--ease-hover); }
  .toggle.is-on { background: var(--primary); box-shadow: none; }
  .toggle.is-on .toggle__knob { background: #fff; transform: translateX(16px); }
  .toggle:hover:not(:disabled):not(.is-on) { background: var(--surface); }
  ```

### 10. The hands-free × is a 20 px target — **Should fix**
- **Where:** `src/popup/popup.css:238-262` (`.pill__close`).
- **What's wrong:** it is the only way to cancel hands-free with the mouse, and it is under the 24 px
  minimum target size.
- **Change:** `width: 24px; height: 24px; margin: 0 -6px 0 -4px;` and keep the 14 px icon. The net
  width is unchanged, so the pill doesn't grow.

### 11. "No sound from your mic" can be clicked, but nothing says so, and it leaves too soon — **Should fix**
- **Where:** `src/popup/Popup.tsx:293-301`; `POPUP_NO_SOUND_MS = 2500` in `src-tauri/src/constants.rs:158`
  and `src/lib/api.ts:85`.
- **What's wrong:** clicking it opens the Windows microphone privacy settings, but nothing hints at
  that, and 2.5 s is too short to read it and act. "Not pasted" handles the same situation well
  (soft hint plus countdown line).
- **Change:**
  - Content → `No sound from your mic <span className="pill__soft">· open settings</span>`.
  - Set `countdownMs = DISMISS_MS.noSound`.
  - Raise `POPUP_NO_SOUND_MS` to `4000` in both places.

### 12. Sidebar status uses jargon and disagrees with Settings — **Should fix**
- **Where:** `src/components/Sidebar.tsx:26-37`, `:56-59`.
- **What's wrong:** the footer says "Ready on GPU", "Ready on CPU" and "Engine error", while Settings
  says "Runs on: Graphics card". It also can't be clicked when something is wrong.
- **Change:**
  - Ready on GPU → `Ready`, with the title `Running on your graphics card`.
  - CPU → `Ready · slower`, with the title `The graphics card isn't available, so speech runs slower`.
  - Failed → `Speech stopped`.
  - When the tone is `warn` or `bad`, render the footer as a `<button>` that opens Settings, scrolled
    to the speech section.

### 13. Settings copy: a duplicate label and wordy rows — **Nice to have**
- **Where:** `src/pages/Settings.tsx:130`, `:278`, `:559`.
- **Change:**
  - The row under the "Microphone" group heading is also titled "Microphone" → rename the row to
    `Which microphone`.
  - Start with Windows → `Turn this on once Willow and Glaido no longer start with Windows, so only one app listens for the keys.`
  - `Set automatically from that check` → `Wren chose this from the check`.

### 14. The popup outline is lost on dark desktops — **Nice to have**
- **Where:** `src/popup/popup.css:46-52` (the `.pill` shadow) and `.hint` (382).
- **What's wrong:** right column of `popup-contact-sheet.png`. The hairline is 6 % white, so on a dark
  taskbar or wallpaper the pill's outline disappears and only the bars float.
- **Change:**
  - In the pill shadow: `inset 0 0 0 1px rgba(255,255,255,0.11)` and `inset 0 1px 0 rgba(255,255,255,0.14)`.
  - Add `box-shadow: inset 0 0 0 1px rgba(255,255,255,0.08), 0 2px 8px rgba(0,0,0,.25)` to `.hint`.

### 15. With reduced motion, "transcribing" looks the same as "listening, no sound yet" — **Nice to have**
- **Where:** `src/popup/popup.css:433-437`.
- **What's wrong:** both are flat grey dots (0.5 and 0.34 opacity) when animation is off, so there is
  no sign that Wren is working.
- **Change:** keep a still highlight instead of removing the mask:
  `.stage.is-reduced .bars--frozen { animation: none; -webkit-mask-position: 50% 0; mask-position: 50% 0; opacity: 1; }`.
  Drop the `mask-image: none` and `opacity: .5` lines. The bright centre band then reads as "working".

### 16. Long learned words can push the notice past the popup window — **Nice to have**
- **Where:** `src/popup/popup.css` `.pill__from` and `.pill__text strong`; the window is 360 px wide
  (`constants.rs:164`).
- **What's wrong:** "Learned rust sequel → rusqlite · Undo" is already about 335 px. Anything longer
  gets cut off at the window edge, and so does its shadow.
- **Change:**
  `.pill--learned .pill__from, .pill--learned strong { display: inline-block; max-width: 96px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; vertical-align: bottom; }`

### 17. Fix-a-word shows its result line before a word is picked — **Nice to have**
- **Where:** `src/components/TranscriptCard.tsx:242`. Seen in `home-fix-pick.png`.
- **What's wrong:** "Wren will spell it this way from now on." sits next to "Click the word Wren got
  wrong." before anything has been chosen.
- **Change:** render `fixbar__note` only when `picked !== null`.

### 18. Disabled primary buttons look like washed-out lilac — **Nice to have**
- **Where:** `src/styles/components.css:188-191`. Seen in "Add word" in `dictionary.png` and "Add" in
  `dictionary-replacements.png`.
- **Change:**
  `.btn--primary:disabled { opacity: 1; background: var(--surface-sunk); color: var(--text-3); box-shadow: none; }`

### 19. Show the main window once it has painted — **Nice to have**
- **Where:** `src-tauri/src/lib.rs:155-158`.
- **What's wrong:** the window is shown during Rust setup, before the web page has loaded. Item 1's
  `backgroundColor` hides the flash, but he still sees an empty canvas, then the sidebar, then the page.
- **Change:**
  - Leave the window hidden in setup.
  - In `App.tsx`, after Home's first `load()` resolves, call `getCurrentWindow().show()`.
  - Keep a Rust fallback that shows it after 1.5 s if the front end never calls.

### 20. The 30-day strip on Home is the only chart without an axis cue — **Nice to have**
- **Where:** `src/components/DayStrip.tsx`.
- **What's wrong:** with one day of data (real `home.png`), there is a single tall indigo bar at the far
  right and grey dashes elsewhere, with nothing saying that the right-hand end is today.
- **Change:** add a tiny right-aligned `Today` label under the last bar: 12 px, `var(--text-3)`, using
  the same style as `.chart__xtick`.

---

## What's already better than both apps (leave alone)
- Figures at 44 px with quieter units; hover shows the formula. Glaido's figures are about 32 px.
- Click-to-copy cards: the hover hint, the "Copied" pill and the ring pulse are clearer than anything
  in Glaido or Willow.
- The Stats page: totals with an all-time highlight, words per day with an average line, the busiest
  hour picked out, and speed against typing speed. Neither app has this.
- The level bars: a fast attack, a slow release, a ripple out from the centre and a slow idle breath.
  They feel alive without jitter. The 1.6 s shimmer for "transcribing" matches Glaido's cue, and the
  spring (ζ≈0.72, about 4 % overshoot) is calm.
- Copy is British, plain and friendly throughout. Empty states teach the hotkey with key caps.
