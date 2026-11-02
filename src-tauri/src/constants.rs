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
/// Hotkey watchdog: if Windows saw input this long after the hook's last event, reinstall the hook.
pub const HOOK_SILENT_REINSTALL_MS: u64 = 5_000;
/// How often the hotkey watchdog compares `GetLastInputInfo` with the hook's last event.
pub const HOOK_WATCHDOG_POLL_MS: u64 = 1_000;
/// Lock/unlock notifications can fail to register at early autostart (Terminal Services not up
/// yet); the hotkey retries this often, this many times, without delaying hook installation.
pub const SESSION_NOTIFY_RETRY_MS: u64 = 2_000;
pub const SESSION_NOTIFY_RETRIES: u32 = 30;
/// A hold that has run at least this long is saved to History (not pasted) when cancelled.
pub const CANCEL_SAVE_AFTER_MS: u64 = 3_000;
/// Transcriptions and their audio are deleted after this many hours.
pub const RETENTION_HOURS: i64 = 72;
/// The retention sweep runs at startup and then this often.
pub const SWEEP_INTERVAL_MS: u64 = 3_600_000;
/// A WAV in the audio folder with no History row is deleted only once it is at least this old,
/// so a recording that is still being saved is never swept away.
pub const ORPHAN_AUDIO_MIN_AGE_MS: u64 = 3_600_000;
/// How long a database call waits for a lock held by another connection before failing.
pub const DB_BUSY_TIMEOUT_MS: u64 = 5_000;
/// Stats window for the daily chart and the busiest hour (spec §9).
pub const STATS_WINDOW_DAYS: i64 = 30;
/// Correction diff (spec §8): a learnable swap covers at most this many words on each side…
pub const LEARN_MAX_RUN_WORDS: usize = 3;
/// …and more separate edits than this in one text means a rewrite, so nothing is learned.
pub const LEARN_MAX_EDITS: usize = 2;
/// Old and new words "spelt alike" when their normalised edit distance is at most this.
pub const LEARN_MAX_SPELLING_DISTANCE: f64 = 0.4;
/// Double Metaphone code length for "sound alike" (the crate default of 4 is too loose for long words).
pub const LEARN_METAPHONE_CODE_LEN: usize = 12;
/// A learned `from` this short (in characters), or a common word, needs repeated sightings…
pub const LEARN_SHORT_FROM_MAX_CHARS: usize = 2;
/// …this many identical sightings, before its replacement becomes active.
pub const LEARN_PROMOTE_SIGHTINGS: u32 = 2;
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

/// Silence guard (spec §6): a phrase repeated more than this many times in a row is collapsed to one…
pub const REPETITION_MAX_REPEATS: usize = 4;
/// …where a "phrase" is at most this many words.
pub const REPETITION_MAX_PHRASE_WORDS: usize = 8;
/// On a short clip, text of at least this many words found inside the model prompt is a prompt echo.
pub const PROMPT_ECHO_MIN_WORDS: usize = 3;

/// Paste (spec §7): while waiting for the chord keys to come up, check this often.
pub const KEYUP_POLL_MS: u64 = 10;
/// After Ctrl+V, check for the clipboard read receipt this often…
pub const PASTE_READ_POLL_MS: u64 = 10;
/// …for at most this long (then Ctrl+Shift+V once, waited for just as long).
pub const PASTE_READ_WAIT_MS: u64 = 1_000;
/// `OpenClipboard` is retried this many times, this far apart, when another app holds it.
pub const CLIPBOARD_OPEN_RETRIES: u32 = 5;
pub const CLIPBOARD_OPEN_RETRY_MS: u64 = 20;
/// A clipboard format bigger than this is not backed up (it is lost on restore instead).
pub const CLIPBOARD_BACKUP_MAX_FORMAT_BYTES: usize = 256 * 1024 * 1024;
/// Longest wait for the clipboard worker to publish Wren's text before giving up.
pub const CLIPBOARD_PUBLISH_TIMEOUT_MS: u64 = 3_000;

/// Hotkey watchdog: reinstall the hook if Windows saw input but the hook saw nothing for this long.
pub const HOOK_WATCHDOG_MS: u64 = 5_000;
/// Correction watcher: first read this long after the paste; one retry this long later if empty.
pub const WATCH_FIRST_READ_MS: u64 = 250;
pub const WATCH_RETRY_MS: u64 = 300;

/// How long each transient popup state stays up (spec §11).
pub const POPUP_PASTED_MS: u64 = 400;
pub const POPUP_STILL_WORKING_AFTER_MS: u64 = 1_500;
pub const POPUP_NOTHING_MS: u64 = 1_500;
pub const POPUP_NO_SOUND_MS: u64 = 2_500;
pub const POPUP_NOT_PASTED_MS: u64 = 4_000;
pub const POPUP_LEARNED_MS: u64 = 6_000;
pub const POPUP_ERROR_MS: u64 = 5_000;

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
    pub popup_pasted_ms: u64,
    pub popup_still_working_after_ms: u64,
    pub popup_nothing_ms: u64,
    pub popup_no_sound_ms: u64,
    pub popup_not_pasted_ms: u64,
    pub popup_learned_ms: u64,
    pub popup_error_ms: u64,
}

pub fn ui_constants() -> UiConstants {
    UiConstants {
        retention_hours: RETENTION_HOURS,
        typing_wpm_default: TYPING_WPM_DEFAULT,
        popup_window_w: POPUP_WINDOW_W,
        popup_window_h: POPUP_WINDOW_H,
        recording_warn_ms: RECORDING_WARN_MS,
        max_recording_ms: MAX_RECORDING_MS,
        popup_pasted_ms: POPUP_PASTED_MS,
        popup_still_working_after_ms: POPUP_STILL_WORKING_AFTER_MS,
        popup_nothing_ms: POPUP_NOTHING_MS,
        popup_no_sound_ms: POPUP_NO_SOUND_MS,
        popup_not_pasted_ms: POPUP_NOT_PASTED_MS,
        popup_learned_ms: POPUP_LEARNED_MS,
        popup_error_ms: POPUP_ERROR_MS,
    }
}
