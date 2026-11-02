//! Every tunable number in Wren lives here (spec §2). Other modules import these;
//! nobody hard-codes a timing or threshold elsewhere.

use serde::Serialize;

/// Left Ctrl virtual-key code.
pub const VK_CHORD_CTRL: u16 = 0xA2;
/// Left Win virtual-key code.
pub const VK_CHORD_WIN: u16 = 0x5B;
/// Unassigned key injected while Win is held so Windows does not open Start on release.
pub const VK_MASK: u16 = 0xE8;
/// Marker put in `dwExtraInfo` of every key Wren injects ("WREN").
pub const INJECT_TAG: usize = 0x5752_454E;

/// Chord released before this many ms is a tap (audio discarded). Matches Willow, which he is used to.
pub const TAP_MAX_MS: u64 = 400;
/// A second chord-down within this many ms of the first release starts hands-free.
pub const DOUBLE_TAP_GAP_MS: u64 = 600;
/// Chord presses are ignored for this long after hands-free starts.
pub const HANDSFREE_STOP_DEBOUNCE_MS: u64 = 500;
/// The popup appears this long after chord-down (so Ctrl+Win+Right never flashes it).
pub const POPUP_DELAY_MS: u64 = 90;
/// Longest wait for both chord keys to be physically up before pasting.
pub const KEYUP_WAIT_MAX_MS: u64 = 2_000;
/// Previous clipboard contents are restored this long after the target app read our text.
pub const CLIPBOARD_RESTORE_MS: u64 = 500;
/// Less detected speech than this means nothing is pasted.
pub const MIN_SPEECH_MS: u64 = 300;
/// Clips shorter than this get the known-hallucination filter.
pub const SHORT_CLIP_MS: u64 = 2_000;
/// Hard cap on any recording (hold or hands-free).
pub const MAX_RECORDING_MS: u64 = 600_000;
/// The popup warns this long into a recording that the cap is near.
pub const RECORDING_WARN_MS: u64 = 540_000;
/// Continuous silence this long ends a hands-free session (saved, not pasted).
pub const HANDSFREE_SILENCE_STOP_MS: u64 = 90_000;
/// While recording, physical key state is polled this often to recover lost key-ups.
pub const KEYSTATE_POLL_MS: u64 = 250;
/// A hold that has run at least this long is saved to History (not pasted) when cancelled.
pub const CANCEL_SAVE_AFTER_MS: u64 = 3_000;
/// Transcriptions and their audio are deleted after this many hours.
pub const RETENTION_HOURS: i64 = 72;
/// Default typing speed used for "time saved".
pub const TYPING_WPM_DEFAULT: u32 = 52;
/// Most dictionary terms sent to the model with one dictation.
pub const BIAS_TERMS_MAX: usize = 50;
/// Correction watcher poll interval.
pub const WATCH_POLL_MS: u64 = 500;
/// Watcher polls this slowly once 10 s have passed without a change.
pub const WATCH_POLL_SLOW_MS: u64 = 2_000;
pub const WATCH_FAST_PHASE_MS: u64 = 10_000;
/// Correction watcher gives up after this long.
pub const WATCH_MAX_MS: u64 = 300_000;
/// A UI Automation call slower than this counts as slow.
pub const WATCH_SLOW_CALL_MS: u64 = 250;
/// Sidecar restart back-off steps; after `SIDECAR_MAX_FAILURES` within `SIDECAR_FAILURE_WINDOW_MS` it stays down.
pub const SIDECAR_RESTART_BACKOFF_MS: [u64; 3] = [1_000, 2_000, 5_000];
pub const SIDECAR_MAX_FAILURES: usize = 5;
pub const SIDECAR_FAILURE_WINDOW_MS: u64 = 300_000;
/// If the engine is still warming up, a finished dictation is pasted only if ready within this long.
pub const WARMUP_PASTE_GRACE_MS: u64 = 5_000;
/// "No sound from mic" if nothing above the silence floor arrives within this long of chord-down.
pub const NO_SOUND_WARN_MS: u64 = 1_500;
/// Audio sample rate handed to the engine.
pub const ENGINE_SAMPLE_RATE: u32 = 16_000;
/// Level events per second sent to the popup.
pub const LEVEL_EVENTS_HZ: u32 = 30;
/// AI clean-up: replies slower than this, or changing length by more than the ratio, are discarded.
pub const AI_CLEANUP_TIMEOUT_MS: u64 = 2_500;
pub const AI_CLEANUP_MAX_LENGTH_CHANGE: f32 = 0.30;

/// Popup window size in logical pixels (the window is larger than the visible pill so it can animate).
pub const POPUP_WINDOW_W: f64 = 360.0;
pub const POPUP_WINDOW_H: f64 = 120.0;
/// Gap between the bottom of the popup window and the bottom of the monitor work area (logical px).
pub const POPUP_BOTTOM_MARGIN: f64 = 12.0;

/// Constants the front end needs, sent once via `get_constants`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConstants {
    pub retention_hours: i64,
    pub typing_wpm_default: u32,
    pub popup_window_w: f64,
    pub popup_window_h: f64,
    pub recording_warn_ms: u64,
    pub max_recording_ms: u64,
}

pub fn ui_constants() -> UiConstants {
    UiConstants {
        retention_hours: RETENTION_HOURS,
        typing_wpm_default: TYPING_WPM_DEFAULT,
        popup_window_w: POPUP_WINDOW_W,
        popup_window_h: POPUP_WINDOW_H,
        recording_warn_ms: RECORDING_WARN_MS,
        max_recording_ms: MAX_RECORDING_MS,
    }
}
