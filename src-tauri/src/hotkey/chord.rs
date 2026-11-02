//! The Ctrl+Win chord machine (spec §3). Pure: no OS calls, no clock, no I/O.
//! Every timing comes from `constants.rs`.

use crate::constants::{
    DOUBLE_TAP_GAP_MS, HANDSFREE_STOP_DEBOUNCE_MS, TAP_MAX_MS, VK_CHORD_CTRL, VK_CHORD_WIN,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDir {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub vk: u16,
    pub dir: KeyDir,
    pub injected: bool,
    pub tagged: bool,
    pub t_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordOutput {
    MaskNow,
    HoldStart,
    HoldEnd { held_ms: u64 },
    Tap,
    HandsFreeStart,
    HandsFreeStop,
    Cancel { held_ms: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    /// Chord held, recording since `since`.
    Held { since: u64 },
    /// A tap just ended at `released_at`; a chord-down within `DOUBLE_TAP_GAP_MS` starts hands-free.
    AwaitDouble { released_at: u64 },
    /// Hands-free recording started at `started_at`.
    HandsFree { started_at: u64 },
}

/// Tracks Left Ctrl, Left Win and every other key that is down, and turns key events into
/// recording decisions. The chord forms only on a chord-key *down* with no other key down,
/// so releasing some other key can never start a recording.
#[derive(Debug, Clone)]
pub struct ChordMachine {
    ctrl: bool,
    win: bool,
    /// Non-chord keys currently down (includes Right Ctrl / Right Win).
    others: Vec<u16>,
    mode: Mode,
}

impl Default for ChordMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl ChordMachine {
    pub fn new() -> Self {
        Self { ctrl: false, win: false, others: Vec::new(), mode: Mode::Idle }
    }

    /// Feeds one key event. Events carrying Wren's own tag are ignored entirely.
    pub fn on_key(&mut self, ev: KeyEvent) -> Vec<ChordOutput> {
        if ev.tagged {
            return Vec::new();
        }
        let t = ev.t_ms;
        match (ev.vk, ev.dir) {
            (VK_CHORD_CTRL | VK_CHORD_WIN, KeyDir::Down) => {
                let slot = self.chord_slot(ev.vk);
                if *slot {
                    return Vec::new(); // auto-repeat
                }
                *slot = true;
                if self.ctrl && self.win && self.others.is_empty() {
                    self.chord_down(t)
                } else {
                    Vec::new()
                }
            }
            (VK_CHORD_CTRL | VK_CHORD_WIN, KeyDir::Up) => {
                let slot = self.chord_slot(ev.vk);
                if !*slot {
                    return Vec::new();
                }
                *slot = false;
                self.chord_key_up(t)
            }
            (vk, KeyDir::Down) => {
                if self.others.contains(&vk) {
                    return Vec::new(); // auto-repeat
                }
                self.others.push(vk);
                match self.mode {
                    Mode::Held { since } => {
                        self.mode = Mode::Idle;
                        vec![ChordOutput::Cancel { held_ms: t.saturating_sub(since) }]
                    }
                    // Typing between two taps means it was not a double-tap.
                    Mode::AwaitDouble { .. } => {
                        self.mode = Mode::Idle;
                        Vec::new()
                    }
                    Mode::Idle | Mode::HandsFree { .. } => Vec::new(),
                }
            }
            (vk, KeyDir::Up) => {
                self.others.retain(|k| *k != vk);
                Vec::new()
            }
        }
    }

    /// Time passing with no key event. Closes an expired double-tap window.
    pub fn on_tick(&mut self, now_ms: u64) -> Vec<ChordOutput> {
        if let Mode::AwaitDouble { released_at } = self.mode {
            if now_ms.saturating_sub(released_at) > DOUBLE_TAP_GAP_MS {
                self.mode = Mode::Idle;
            }
        }
        Vec::new()
    }

    /// Physical state of both chord keys (polled while recording). A key the machine thinks is
    /// down but is physically up had its key-up lost; it is released now.
    pub fn on_key_state(&mut self, ctrl_down: bool, win_down: bool, now_ms: u64) -> Vec<ChordOutput> {
        let mut out = self.sync_key(VK_CHORD_CTRL, ctrl_down, now_ms);
        out.extend(self.sync_key(VK_CHORD_WIN, win_down, now_ms));
        out
    }

    /// Brings one chord key in line with its physical state. A lost key-up is replayed as a
    /// key-up at `now_ms`; a missed key-down is recorded silently (it never starts the chord).
    pub fn sync_key(&mut self, vk: u16, down: bool, now_ms: u64) -> Vec<ChordOutput> {
        if vk != VK_CHORD_CTRL && vk != VK_CHORD_WIN {
            return Vec::new();
        }
        let slot = self.chord_slot(vk);
        match (*slot, down) {
            (true, false) => {
                *slot = false;
                self.chord_key_up(now_ms)
            }
            (false, true) => {
                *slot = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Forgets non-chord keys that are no longer physically down (their key-up was lost).
    pub fn prune_other_keys(&mut self, is_down: impl Fn(u16) -> bool) {
        self.others.retain(|vk| is_down(*vk));
    }

    /// The session is ending under us (lock, sleep, disconnect). A hold is cancelled, hands-free
    /// is stopped, and every key is forgotten because their key-ups will not arrive.
    pub fn on_session_interrupted(&mut self, now_ms: u64) -> Vec<ChordOutput> {
        let out = match self.mode {
            Mode::Held { since } => vec![ChordOutput::Cancel { held_ms: now_ms.saturating_sub(since) }],
            Mode::HandsFree { .. } => vec![ChordOutput::HandsFreeStop],
            Mode::Idle | Mode::AwaitDouble { .. } => Vec::new(),
        };
        self.force_idle();
        out
    }

    /// Back to idle with no output, forgetting every key (the session decided to stop).
    pub fn force_idle(&mut self) {
        *self = Self::new();
    }

    pub fn is_recording(&self) -> bool {
        matches!(self.mode, Mode::Held { .. } | Mode::HandsFree { .. })
    }

    fn chord_slot(&mut self, vk: u16) -> &mut bool {
        if vk == VK_CHORD_CTRL { &mut self.ctrl } else { &mut self.win }
    }

    /// Both chord keys are down and nothing else is.
    fn chord_down(&mut self, t: u64) -> Vec<ChordOutput> {
        let (next, event) = match self.mode {
            Mode::Idle => (Mode::Held { since: t }, Some(ChordOutput::HoldStart)),
            Mode::AwaitDouble { released_at } if t.saturating_sub(released_at) <= DOUBLE_TAP_GAP_MS => {
                (Mode::HandsFree { started_at: t }, Some(ChordOutput::HandsFreeStart))
            }
            Mode::AwaitDouble { .. } => (Mode::Held { since: t }, Some(ChordOutput::HoldStart)),
            Mode::HandsFree { started_at } if t.saturating_sub(started_at) >= HANDSFREE_STOP_DEBOUNCE_MS => {
                (Mode::Idle, Some(ChordOutput::HandsFreeStop))
            }
            // Inside the debounce: ignored (the mask is still sent so Start stays shut).
            m @ Mode::HandsFree { .. } => (m, None),
            // Cannot happen (both keys were already down), but stay put if it does.
            m @ Mode::Held { .. } => (m, None),
        };
        self.mode = next;
        let mut out = vec![ChordOutput::MaskNow];
        out.extend(event);
        out
    }

    /// The first chord key came up.
    fn chord_key_up(&mut self, t: u64) -> Vec<ChordOutput> {
        let Mode::Held { since } = self.mode else {
            return Vec::new();
        };
        let held_ms = t.saturating_sub(since);
        if held_ms < TAP_MAX_MS {
            self.mode = Mode::AwaitDouble { released_at: t };
            vec![ChordOutput::Tap]
        } else {
            self.mode = Mode::Idle;
            vec![ChordOutput::HoldEnd { held_ms }]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ChordOutput::*;
    use super::*;

    const SHIFT: u16 = 0xA0;
    const ALT: u16 = 0xA4;
    const RIGHT: u16 = 0x27;
    const ENTER: u16 = 0x0D;
    const KEY_S: u16 = 0x53;
    const RCTRL: u16 = 0xA3;
    const RWIN: u16 = 0x5C;
    const CTRL: u16 = VK_CHORD_CTRL;
    const WIN: u16 = VK_CHORD_WIN;

    fn ev(vk: u16, dir: KeyDir, t_ms: u64) -> KeyEvent {
        KeyEvent { vk, dir, injected: false, tagged: false, t_ms }
    }
    fn down(vk: u16, t: u64) -> KeyEvent {
        ev(vk, KeyDir::Down, t)
    }
    fn up(vk: u16, t: u64) -> KeyEvent {
        ev(vk, KeyDir::Up, t)
    }

    /// Feeds events in order and returns every output, flattened.
    fn feed(m: &mut ChordMachine, evs: &[KeyEvent]) -> Vec<ChordOutput> {
        evs.iter().flat_map(|e| m.on_key(*e)).collect()
    }

    /// A full chord press: Ctrl at `t`, Win at `t+10`, both released at `t+10+held` (Win first).
    fn press(m: &mut ChordMachine, t: u64, held: u64) -> Vec<ChordOutput> {
        feed(m, &[down(CTRL, t), down(WIN, t + 10), up(WIN, t + 10 + held), up(CTRL, t + 15 + held)])
    }

    /// Outputs without the mask, for tests about the recording outcome.
    fn no_mask(v: Vec<ChordOutput>) -> Vec<ChordOutput> {
        v.into_iter().filter(|o| *o != MaskNow).collect()
    }

    #[test]
    fn hold_start_on_chord_down_either_order() {
        let mut m = ChordMachine::new();
        assert_eq!(m.on_key(down(CTRL, 0)), vec![]);
        assert_eq!(m.on_key(down(WIN, 20)), vec![MaskNow, HoldStart]);
        assert!(m.is_recording());

        let mut m = ChordMachine::new();
        assert_eq!(m.on_key(down(WIN, 0)), vec![]);
        assert_eq!(m.on_key(down(CTRL, 20)), vec![MaskNow, HoldStart]);
        assert!(m.is_recording());
    }

    #[test]
    fn hold_end_after_tap_max() {
        // Released exactly at TAP_MAX_MS: a hold, not a tap.
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 100)]);
        assert_eq!(m.on_key(up(WIN, 100 + TAP_MAX_MS)), vec![HoldEnd { held_ms: TAP_MAX_MS }]);
        assert!(!m.is_recording());
        assert_eq!(m.on_key(up(CTRL, 100 + TAP_MAX_MS + 30)), vec![]);

        // Ctrl released first also ends the hold.
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(WIN, 0), down(CTRL, 5)]);
        assert_eq!(m.on_key(up(CTRL, 5 + 2_000)), vec![HoldEnd { held_ms: 2_000 }]);
        assert_eq!(m.on_key(up(WIN, 5 + 2_010)), vec![]);
    }

    #[test]
    fn tap_under_tap_max_discards() {
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert_eq!(m.on_key(up(WIN, 10 + TAP_MAX_MS - 1)), vec![Tap]);
        assert!(!m.is_recording());
        assert_eq!(m.on_key(up(CTRL, 10 + TAP_MAX_MS + 5)), vec![]);
    }

    #[test]
    fn double_tap_within_gap_starts_handsfree() {
        let mut m = ChordMachine::new();
        assert_eq!(no_mask(press(&mut m, 0, 100)), vec![HoldStart, Tap]);
        // First release at t=110; second chord-down exactly DOUBLE_TAP_GAP_MS later.
        let t2 = 110 + DOUBLE_TAP_GAP_MS;
        assert_eq!(m.on_key(down(CTRL, t2 - 10)), vec![]);
        assert_eq!(m.on_key(down(WIN, t2)), vec![MaskNow, HandsFreeStart]);
        assert!(m.is_recording());
        // Releasing the chord does not stop hands-free.
        assert_eq!(feed(&mut m, &[up(WIN, t2 + 80), up(CTRL, t2 + 90)]), vec![]);
        assert!(m.is_recording());
    }

    #[test]
    fn second_tap_after_gap_is_new_hold() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100); // released at 110
        let t2 = 110 + DOUBLE_TAP_GAP_MS + 1;
        assert_eq!(feed(&mut m, &[down(CTRL, t2 - 10), down(WIN, t2)]), vec![MaskNow, HoldStart]);
        assert_eq!(m.on_key(up(WIN, t2 + 1_000)), vec![HoldEnd { held_ms: 1_000 }]);
    }

    #[test]
    fn second_tap_after_gap_detected_by_on_tick_too() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100); // released at 110
        assert_eq!(m.on_tick(110 + DOUBLE_TAP_GAP_MS + 50), vec![]);
        let t2 = 110 + DOUBLE_TAP_GAP_MS + 60;
        assert_eq!(no_mask(feed(&mut m, &[down(CTRL, t2), down(WIN, t2 + 5)])), vec![HoldStart]);
    }

    #[test]
    fn handsfree_stops_on_next_chord_after_debounce() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100); // release at 110
        feed(&mut m, &[down(CTRL, 300), down(WIN, 310)]); // HandsFreeStart at 310
        feed(&mut m, &[up(WIN, 380), up(CTRL, 390)]);
        // A press inside the debounce is ignored (only the mask).
        let early = 310 + HANDSFREE_STOP_DEBOUNCE_MS - 1;
        assert_eq!(feed(&mut m, &[down(CTRL, early - 10), down(WIN, early)]), vec![MaskNow]);
        assert!(m.is_recording());
        feed(&mut m, &[up(WIN, early + 30), up(CTRL, early + 40)]);
        // The next press after the debounce stops.
        let late = 310 + HANDSFREE_STOP_DEBOUNCE_MS + 200;
        assert_eq!(feed(&mut m, &[down(WIN, late - 10), down(CTRL, late)]), vec![MaskNow, HandsFreeStop]);
        assert!(!m.is_recording());
        // Releasing after the stop does nothing, and the next press is a fresh hold.
        assert_eq!(feed(&mut m, &[up(WIN, late + 100), up(CTRL, late + 110)]), vec![]);
        assert_eq!(no_mask(press(&mut m, late + 2_000, 600)), vec![HoldStart, HoldEnd { held_ms: 600 }]);
    }

    #[test]
    fn handsfree_stop_exactly_at_debounce() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100);
        feed(&mut m, &[down(CTRL, 300), down(WIN, 310), up(WIN, 350), up(CTRL, 360)]);
        let t = 310 + HANDSFREE_STOP_DEBOUNCE_MS;
        assert_eq!(feed(&mut m, &[down(CTRL, t - 5), down(WIN, t)]), vec![MaskNow, HandsFreeStop]);
    }

    #[test]
    fn other_key_during_hold_cancels() {
        for other in [SHIFT, ALT, RIGHT, ENTER, KEY_S] {
            let mut m = ChordMachine::new();
            feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
            assert_eq!(m.on_key(down(other, 60)), vec![Cancel { held_ms: 50 }], "vk {other:#x}");
            assert!(!m.is_recording(), "vk {other:#x}");
            // The rest of the shortcut produces nothing: no HoldEnd, no Tap, no restart.
            let rest = feed(&mut m, &[up(other, 90), up(WIN, 100), up(CTRL, 110)]);
            assert_eq!(rest, vec![], "vk {other:#x}");
        }
    }

    #[test]
    fn releasing_the_other_key_never_restarts_the_chord() {
        // Ctrl+Win+Right, Right released while Ctrl+Win stay down: a release never starts a chord.
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10), down(RIGHT, 30)]);
        assert_eq!(m.on_key(up(RIGHT, 60)), vec![]);
        assert!(!m.is_recording());
        // Pressing Right again while still held: not recording, nothing to cancel.
        assert_eq!(feed(&mut m, &[down(RIGHT, 90), up(RIGHT, 120)]), vec![]);
    }

    #[test]
    fn long_cancel_reports_held_ms() {
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert_eq!(m.on_key(down(KEY_S, 4_010)), vec![Cancel { held_ms: 4_000 }]);
    }

    #[test]
    fn other_key_already_down_blocks_chord() {
        let mut m = ChordMachine::new();
        // Ctrl+Shift+Win: Shift is down when the chord would complete.
        assert_eq!(feed(&mut m, &[down(CTRL, 0), down(SHIFT, 5), down(WIN, 10)]), vec![]);
        assert!(!m.is_recording());
        // Releasing Shift does not start a chord either.
        assert_eq!(m.on_key(up(SHIFT, 50)), vec![]);
        assert!(!m.is_recording());
    }

    #[test]
    fn other_keys_during_handsfree_do_not_cancel() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100);
        feed(&mut m, &[down(CTRL, 300), down(WIN, 310)]);
        assert!(m.is_recording());
        // While the starting chord is still held, and after it is released.
        let mut outs = feed(&mut m, &[down(SHIFT, 330), up(SHIFT, 340)]);
        outs.extend(feed(&mut m, &[up(WIN, 400), up(CTRL, 410)]));
        for (i, k) in [KEY_S, ENTER, RIGHT, ALT].iter().enumerate() {
            let t = 1_000 + 100 * i as u64;
            outs.extend(feed(&mut m, &[down(*k, t), up(*k, t + 40)]));
        }
        // Ctrl alone (e.g. Ctrl+S while dictating) does not stop or cancel.
        outs.extend(feed(&mut m, &[down(CTRL, 2_000), down(KEY_S, 2_010), up(KEY_S, 2_050), up(CTRL, 2_060)]));
        assert_eq!(outs, vec![]);
        assert!(m.is_recording());
    }

    #[test]
    fn right_ctrl_or_right_win_never_chord() {
        for (a, b) in [(RCTRL, WIN), (CTRL, RWIN), (RCTRL, RWIN), (RWIN, RCTRL)] {
            let mut m = ChordMachine::new();
            let outs = feed(&mut m, &[down(a, 0), down(b, 10), up(b, 600), up(a, 610)]);
            assert_eq!(outs, vec![], "{a:#x}+{b:#x}");
            assert!(!m.is_recording());
        }
        // Right Ctrl pressed during a hold is just another key: it cancels.
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert_eq!(m.on_key(down(RCTRL, 100)), vec![Cancel { held_ms: 90 }]);
    }

    #[test]
    fn autorepeat_downs_ignored() {
        let mut m = ChordMachine::new();
        let mut outs = feed(&mut m, &[down(CTRL, 0), down(CTRL, 30), down(CTRL, 60)]);
        outs.extend(feed(&mut m, &[down(WIN, 70)]));
        assert_eq!(outs, vec![MaskNow, HoldStart]);
        // Windows auto-repeats held keys every ~33 ms; none of that changes anything.
        for t in (100..1_000).step_by(33) {
            assert_eq!(feed(&mut m, &[down(WIN, t), down(CTRL, t + 1)]), vec![]);
        }
        assert!(m.is_recording());
        assert_eq!(m.on_key(up(WIN, 1_070)), vec![HoldEnd { held_ms: 1_000 }]);
    }

    #[test]
    fn tagged_events_ignored() {
        let tagged = |vk, dir, t| KeyEvent { vk, dir, injected: true, tagged: true, t_ms: t };
        let mut m = ChordMachine::new();
        // A tagged chord does nothing.
        assert_eq!(m.on_key(tagged(CTRL, KeyDir::Down, 0)), vec![]);
        assert_eq!(m.on_key(tagged(WIN, KeyDir::Down, 10)), vec![]);
        assert!(!m.is_recording());
        // Wren's own mask key and Ctrl+V during a real hold do not cancel it.
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        let mut outs = vec![];
        for e in [
            tagged(0xE8, KeyDir::Down, 12),
            tagged(0xE8, KeyDir::Up, 13),
            tagged(0x56, KeyDir::Down, 20),
            tagged(0x56, KeyDir::Up, 21),
            tagged(WIN, KeyDir::Up, 22),
        ] {
            outs.extend(m.on_key(e));
        }
        assert_eq!(outs, vec![]);
        assert!(m.is_recording());
        assert_eq!(m.on_key(up(WIN, 900)), vec![HoldEnd { held_ms: 890 }]);
    }

    #[test]
    fn untagged_injected_events_accepted() {
        let inj = |vk, dir, t| KeyEvent { vk, dir, injected: true, tagged: false, t_ms: t };
        let mut m = ChordMachine::new();
        let mut outs = m.on_key(inj(CTRL, KeyDir::Down, 0));
        outs.extend(m.on_key(inj(WIN, KeyDir::Down, 10)));
        outs.extend(m.on_key(inj(WIN, KeyDir::Up, 510)));
        assert_eq!(outs, vec![MaskNow, HoldStart, HoldEnd { held_ms: 500 }]);
    }

    #[test]
    fn mask_emitted_on_every_chord_down() {
        let mut m = ChordMachine::new();
        let mut masks = 0;
        // hold, tap, hands-free start, debounced press, hands-free stop, cancelled press
        let seq: Vec<Vec<KeyEvent>> = vec![
            vec![down(CTRL, 0), down(WIN, 10), up(WIN, 1_000), up(CTRL, 1_010)],
            vec![down(CTRL, 3_000), down(WIN, 3_010), up(WIN, 3_100), up(CTRL, 3_110)],
            vec![down(CTRL, 3_300), down(WIN, 3_310), up(WIN, 3_350), up(CTRL, 3_360)],
            vec![down(CTRL, 3_400), down(WIN, 3_410), up(WIN, 3_450), up(CTRL, 3_460)],
            vec![down(CTRL, 5_000), down(WIN, 5_010), up(WIN, 5_050), up(CTRL, 5_060)],
            vec![down(CTRL, 9_000), down(WIN, 9_010), down(RIGHT, 9_020), up(RIGHT, 9_030), up(WIN, 9_040), up(CTRL, 9_050)],
        ];
        for s in &seq {
            let outs = feed(&mut m, s);
            assert_eq!(outs.first(), Some(&MaskNow), "{outs:?}");
            masks += outs.iter().filter(|o| **o == MaskNow).count();
        }
        assert_eq!(masks, seq.len());
    }

    #[test]
    fn force_idle_resets() {
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert!(m.is_recording());
        m.force_idle();
        assert!(!m.is_recording());
        // The rest of that press produces nothing.
        assert_eq!(feed(&mut m, &[up(WIN, 900), up(CTRL, 910)]), vec![]);
        // Hands-free is reset too, and the next press is a normal hold.
        press(&mut m, 2_000, 100);
        feed(&mut m, &[down(CTRL, 2_300), down(WIN, 2_310)]);
        assert!(m.is_recording());
        m.force_idle();
        assert!(!m.is_recording());
        assert_eq!(feed(&mut m, &[up(WIN, 2_400), up(CTRL, 2_410)]), vec![]);
        assert_eq!(no_mask(press(&mut m, 5_000, 700)), vec![HoldStart, HoldEnd { held_ms: 700 }]);
    }

    #[test]
    fn force_idle_clears_the_double_tap_window() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100);
        m.force_idle();
        assert_eq!(no_mask(feed(&mut m, &[down(CTRL, 200), down(WIN, 210)])), vec![HoldStart]);
    }

    #[test]
    fn lost_keyup_recovered_by_on_key_state() {
        // Win's key-up never reaches the hook (e.g. released over an elevated window).
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert_eq!(m.on_key_state(true, true, 260), vec![]);
        assert!(m.is_recording());
        assert_eq!(m.on_key_state(true, false, 1_010), vec![HoldEnd { held_ms: 1_000 }]);
        assert!(!m.is_recording());
        // Ctrl's key-up is lost too; the next poll clears it with no output.
        assert_eq!(m.on_key_state(false, false, 1_260), vec![]);
        // A lone Win press now must not count as the chord.
        assert_eq!(feed(&mut m, &[down(WIN, 2_000), up(WIN, 2_100)]), vec![]);
    }

    #[test]
    fn lost_keyup_on_a_quick_press_is_a_tap() {
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert_eq!(m.on_key_state(false, false, 250), vec![Tap]);
        assert!(!m.is_recording());
    }

    #[test]
    fn sync_key_marks_a_missed_down_without_starting() {
        // After force_idle the machine forgot Ctrl, but Ctrl is still physically held.
        let mut m = ChordMachine::new();
        m.on_key(down(CTRL, 0));
        m.force_idle();
        assert_eq!(m.sync_key(CTRL, true, 50), vec![]);
        assert!(!m.is_recording());
        assert_eq!(m.on_key(down(WIN, 60)), vec![MaskNow, HoldStart]);
    }

    #[test]
    fn sync_key_clears_a_stale_partner_so_lone_win_is_not_the_chord() {
        // Ctrl's key-up was lost while idle; then he presses Win alone (Win+E).
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(KEY_S, 5), up(KEY_S, 20)]);
        // The hook asks for Ctrl's physical state before feeding Win's down.
        assert_eq!(m.sync_key(CTRL, false, 5_000), vec![]);
        assert_eq!(m.on_key(down(WIN, 5_000)), vec![]);
        assert!(!m.is_recording());
    }

    #[test]
    fn prune_other_keys_drops_stuck_keys() {
        let mut m = ChordMachine::new();
        // Shift's key-up was lost: it would block the chord forever.
        m.on_key(down(SHIFT, 0));
        assert_eq!(feed(&mut m, &[down(CTRL, 1_000), down(WIN, 1_010)]), vec![]);
        feed(&mut m, &[up(WIN, 1_100), up(CTRL, 1_110)]);
        m.prune_other_keys(|vk| vk != SHIFT);
        assert_eq!(feed(&mut m, &[down(CTRL, 2_000), down(WIN, 2_010)]), vec![MaskNow, HoldStart]);
    }

    #[test]
    fn prune_other_keys_keeps_keys_still_down() {
        let mut m = ChordMachine::new();
        m.on_key(down(SHIFT, 0));
        m.prune_other_keys(|_| true);
        assert_eq!(feed(&mut m, &[down(CTRL, 10), down(WIN, 20)]), vec![]);
    }

    #[test]
    fn other_key_between_taps_resets_double_tap() {
        let mut m = ChordMachine::new();
        press(&mut m, 0, 100); // tap, released at 110
        feed(&mut m, &[down(KEY_S, 150), up(KEY_S, 170)]);
        assert_eq!(no_mask(feed(&mut m, &[down(CTRL, 300), down(WIN, 310)])), vec![HoldStart]);
    }

    #[test]
    fn session_interrupted_stops_or_cancels() {
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 0), down(WIN, 10)]);
        assert_eq!(m.on_session_interrupted(5_010), vec![Cancel { held_ms: 5_000 }]);
        assert!(!m.is_recording());

        let mut m = ChordMachine::new();
        press(&mut m, 0, 100);
        feed(&mut m, &[down(CTRL, 300), down(WIN, 310), up(WIN, 350), up(CTRL, 360)]);
        assert_eq!(m.on_session_interrupted(9_000), vec![HandsFreeStop]);
        assert!(!m.is_recording());

        let mut m = ChordMachine::new();
        assert_eq!(m.on_session_interrupted(100), vec![]);
        // Keys held across the lock are forgotten: a lone Win after unlock is not the chord.
        let mut m = ChordMachine::new();
        m.on_key(down(CTRL, 0));
        m.on_session_interrupted(10);
        assert_eq!(m.on_key(down(WIN, 20)), vec![]);
    }

    #[test]
    fn time_going_backwards_does_not_panic() {
        let mut m = ChordMachine::new();
        feed(&mut m, &[down(CTRL, 1_000), down(WIN, 1_010)]);
        assert_eq!(m.on_key(up(WIN, 500)), vec![Tap]);
        assert_eq!(m.on_tick(0), vec![]);
    }
}
