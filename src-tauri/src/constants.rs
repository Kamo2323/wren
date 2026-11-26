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
/// Meeting audio (me.wav / them.wav) is deleted this many hours after the meeting ends (meetings spec §5.5).
pub const MEETING_AUDIO_RETENTION_HOURS: i64 = 24;
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
/// AI clean-up (Claude): the whole request must answer within this long, plus
/// `AI_CLEANUP_TIMEOUT_PER_WORD_MS` per word, at most `AI_CLEANUP_TIMEOUT_MAX_MS`; otherwise the
/// plain text is pasted. Measured on this PC with Haiku 5.5 (2026-11-26): short dictations
/// 0.4–0.6 s, a 190-word coding rewrite 1.4 s (~280 output tokens a second).
pub const AI_CLEANUP_TIMEOUT_MS: u64 = 4_000;
pub const AI_CLEANUP_TIMEOUT_PER_WORD_MS: u64 = 6;
pub const AI_CLEANUP_TIMEOUT_MAX_MS: u64 = 10_000;
/// Strict check: replies changing length by more than this ratio are discarded.
pub const AI_CLEANUP_MAX_LENGTH_CHANGE: f32 = 0.30;
/// Coding "Rewrite as a clear prompt": the reply may be this short or this long (relative to the
/// tidied text) — ums, repeats and false starts may go; backticks and list marks may be added…
pub const AI_REWRITE_MIN_LENGTH_RATIO: f32 = 0.40;
pub const AI_REWRITE_MAX_LENGTH_RATIO: f32 = 1.40;
/// …and at least this share of the reply's words must come from the tidied text.
pub const AI_REWRITE_MIN_REPLY_OVERLAP: f32 = 0.75;
/// …and more than half of the dictation's words (stutters counted once) must still be in the reply,
/// so an answer to the dictation ("The capital of France is Paris.") isn't pasted in its place.
pub const AI_REWRITE_MIN_KEPT: f32 = 0.55;
/// Spoken slash commands in coding apps: a command name is at most this many words.
pub const SLASH_MAX_NAME_WORDS: usize = 4;

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
/// Session (Task 8): wait for one transcription result at most this long plus
/// `TRANSCRIBE_TIMEOUT_PER_CLIP` × the clip length (a 10 min clip on the GPU takes well under a
/// minute; the CPU fallback can take close to real time; this only catches a hung sidecar).
pub const TRANSCRIBE_TIMEOUT_MS: u64 = 60_000;
pub const TRANSCRIBE_TIMEOUT_PER_CLIP: f64 = 1.5;
/// On quit: a recording or queued dictation is transcribed only if the engine is ready within this
/// long; otherwise its audio is kept and a "not transcribed" row saved…
pub const QUIT_ENGINE_WAIT_MS: u64 = 5_000;
/// …and quitting waits at most this long in total for queued dictations.
pub const QUIT_DRAIN_MAX_MS: u64 = 10_000;
/// A start-up problem (hotkey, microphone) is shown in the popup this long after launch, once the
/// popup page has loaded.
pub const STARTUP_ERROR_POPUP_DELAY_MS: u64 = 3_000;
/// The popup plays a ~200 ms exit after its own dismiss duration; the session hides the window this
/// long after that duration (contract with src/popup/Popup.tsx).
pub const POPUP_HIDE_AFTER_DISMISS_MS: u64 = 250;
/// Optional start/stop sounds (`sounds` setting): a soft sine tick this long…
pub const SOUND_TICK_MS: u64 = 40;
/// …at this peak amplitude (0..1 full scale)…
pub const SOUND_TICK_AMPLITUDE: f32 = 0.12;
/// …and these pitches (Hz) for start and stop.
pub const SOUND_START_HZ: f32 = 880.0;
pub const SOUND_STOP_HZ: f32 = 660.0;
/// `--selftest`: the microphone check records this long.
pub const SELFTEST_MIC_MS: u64 = 400;

/// Correction watcher: first read this long after the paste; one retry this long later if empty.
pub const WATCH_FIRST_READ_MS: u64 = 250;
pub const WATCH_RETRY_MS: u64 = 300;

/// How long each transient popup state stays up (spec §11).
pub const POPUP_PASTED_MS: u64 = 400;
pub const POPUP_STILL_WORKING_AFTER_MS: u64 = 1_500;
pub const POPUP_NOTHING_MS: u64 = 1_500;
pub const POPUP_NO_SOUND_MS: u64 = 4_000;
pub const POPUP_NOT_PASTED_MS: u64 = 4_000;
pub const POPUP_LEARNED_MS: u64 = 6_000;
pub const POPUP_ERROR_MS: u64 = 5_000;

/// Popup window size in logical pixels (the window is larger than the visible pill so it can animate).
pub const POPUP_WINDOW_W: f64 = 360.0;
pub const POPUP_WINDOW_H: f64 = 120.0;
/// Gap between the bottom of the popup window and the bottom edge of the monitor (logical px). With
/// the page's 14 px bottom padding the pill's bottom sits 33 px above the monitor's edge: on the top
/// edge of a bottom taskbar, where Glaido puts its pill, rather than over the app being typed into.
pub const POPUP_BOTTOM_MARGIN: f64 = 19.0;
/// While a clickable popup state shows, how often the cursor is checked against the pill so that only
/// the pill (not the whole transparent window) catches clicks.
pub const POPUP_HOVER_POLL_MS: u64 = 33;
/// Extra room around the pill that still counts as "over the pill" (logical px).
pub const POPUP_HIT_SLOP: f64 = 4.0;
/// If the main window's page never says it has painted, show the window anyway after this long.
pub const MAIN_SHOW_FALLBACK_MS: u64 = 1_500;

/// Constants the front end needs, sent once via `get_constants`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiConstants {
    pub platform: &'static str,
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
        platform: if cfg!(target_os = "macos") { "macos" } else { "windows" },
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

// ---- Audio (spec §4) and speech engine supervision (spec §5) ----

/// RMS below this many dBFS counts as silence. Drives `Recording::heard_sound`, "No sound from mic"
/// and the hands-free silence auto-stop.
pub const AUDIO_SILENCE_DBFS: f32 = -50.0;
/// Level meter range: this many dBFS shows as 0…
pub const LEVEL_FLOOR_DBFS: f32 = -60.0;
/// …and this many as 1 (linear in dB between, which reads as even to the ear).
pub const LEVEL_CEIL_DBFS: f32 = -12.0;
/// The `WREN_TEST_AUDIO` seam streams its file in chunks this long, in real time.
pub const TEST_AUDIO_CHUNK_MS: u64 = 10;
/// A sidecar that has not sent `ready` this long after starting is killed and counted as a failure.
pub const SIDECAR_READY_TIMEOUT_MS: u64 = 90_000;
/// After `shutdown`, the sidecar gets this long to exit before its Job Object is terminated.
pub const SIDECAR_SHUTDOWN_GRACE_MS: u64 = 2_000;
/// This many crashes in a row before `ready` (e.g. CUDA aborting the process) → restart on the CPU model.
pub const SIDECAR_CPU_AFTER_PRE_READY_CRASHES: usize = 2;
/// How often the supervisor checks the sidecar process.
pub const SIDECAR_POLL_MS: u64 = 50;

// ---- Correction watcher, pre-paste check, AI clean-up (Task 9; spec §8, §10, ruling 24) ----

/// Watcher: this many UI Automation failures in a row end the watch.
pub const WATCH_MAX_FAILURES: u32 = 3;
/// Watcher: this many slow calls (> `WATCH_SLOW_CALL_MS`) switch the watcher off for that app.
pub const WATCH_MAX_SLOW_CALLS: u32 = 3;
/// Watcher: a single UI Automation request is abandoned by Windows after this long.
pub const WATCH_UIA_TIMEOUT_MS: u64 = 1_500;
/// Watcher: while waiting between reads, check this often whether the watch was stopped.
pub const WATCH_STOP_CHECK_MS: u64 = 25;
/// Watcher: dictations shorter than this (characters) are not watched.
pub const WATCH_MIN_PASTE_CHARS: usize = 3;
/// Watcher: a text box longer than this many times the dictation (or `WATCH_MAX_FIELD_CHARS`,
/// whichever is larger) is not a box worth watching.
pub const WATCH_MAX_FIELD_FACTOR: usize = 10;
pub const WATCH_MAX_FIELD_CHARS: usize = 20_000;
/// "Still resembles" (Willow's measure): the shorter text is at least this fraction of the longer…
pub const WATCH_SIMILAR_MIN_LEN_RATIO: f64 = 0.25;
/// …texts up to this many characters compare by normalised Levenshtein similarity…
pub const WATCH_SIMILAR_SHORT_CHARS: usize = 64;
pub const WATCH_SIMILAR_MIN_LEVENSHTEIN: f64 = 0.3;
/// …longer ones by word-bigram (or character-4-gram) Dice coefficient.
pub const WATCH_SIMILAR_MIN_DICE: f64 = 0.4;
/// Pre-paste editable check (ruling 24): the whole UI Automation question must answer within this
/// long, or Wren pastes anyway.
pub const PREPASTE_CHECK_MAX_MS: u64 = 50;

/// AI clean-up: longest reply, in tokens (thinking at low effort counts too): this floor plus this
/// many per word of the input, capped.
pub const AI_CLEANUP_MIN_TOKENS: usize = 1_024;
pub const AI_CLEANUP_TOKENS_PER_WORD: usize = 4;
pub const AI_CLEANUP_MAX_TOKENS_CAP: usize = 8_192;
/// The fixed prompt + worked examples are marked for caching only when at least this long (in
/// estimated tokens): the smallest prefix Haiku 5.5 / Sonnet 5.5 cache.
pub const AI_CLEANUP_CACHE_MIN_TOKENS: usize = 512;
/// AI clean-up guard (on top of the length rule): at least this share of the tidied text's words
/// must still be in the reply, and of the reply's words in the tidied text (case-insensitive).
pub const AI_CLEANUP_MIN_WORD_OVERLAP: f32 = 0.8;
/// A reply that starts with a lead-in ending in ':' or a line break within this many characters,
/// which the tidied text does not start with, is chatter ("Here is the corrected transcript:").
pub const AI_CLEANUP_LEAD_IN_MAX_CHARS: usize = 60;

// ---- App updater (GitHub Releases) ----

/// GitHub repository that publishes the official signed Wren releases (`latest.json` + installers).
/// A build can point elsewhere (a fork's own releases) or turn updates off with `WREN_UPDATE_REPO`
/// at build time; see `updater::UpdateSource`.
pub const UPDATE_REPO_DEFAULT: &str = "Kamo2323/wren";
/// Base64-encoded minisign public key matching the `TAURI_SIGNING_PRIVATE_KEY` GitHub Actions secret.
pub const UPDATE_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDEzRjQzQkQ1MEQ3NTc4RDcKUldUWGVIVU4xVHYwRTBMTHFTNFFHQUpNczUwM2p4MW14V1lkc1R2UWNidHVQbEpwZzlCeVkvcFoK";
/// Wait this long after start-up before the first silent update check, so start-up and engine
/// warm-up are never delayed.
pub const UPDATE_FIRST_CHECK_DELAY_MS: u64 = 15_000;
/// Silent background update checks repeat this often.
pub const UPDATE_CHECK_INTERVAL_MS: u64 = 3_600_000;
/// Timeout for checking release metadata on GitHub.
pub const UPDATE_CHECK_TIMEOUT_MS: u64 = 15_000;
/// Timeout for downloading the Windows installer from GitHub Releases.
pub const UPDATE_DOWNLOAD_TIMEOUT_MS: u64 = 300_000;
/// Timeout for reading a GitHub token via `gh auth token` when the repository is private.
pub const UPDATE_GH_TOKEN_TIMEOUT_MS: u64 = 5_000;
/// Emit download progress events to the UI at most this often.
pub const UPDATE_PROGRESS_EMIT_MS: u64 = 100;
/// Short grace period before quitting so the front end receives the "installing" reply cleanly.
pub const UPDATE_EXIT_DELAY_MS: u64 = 150;
/// Seconds the detached update helper waits for the current `wren.exe` to exit cleanly.
pub const UPDATE_WAIT_PARENT_SECS: u64 = 25;
