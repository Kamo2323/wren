//! `wren --selftest` (spec §13): checks the parts a dictation needs, prints one line per check and
//! then `SELFTEST PASS` or `SELFTEST FAIL <reasons>`, and exits 0 / 1. Runs without opening any
//! window and never sends a key.
//!
//! Checks: the keyboard hook installs; a microphone is present and records (or the `WREN_TEST_AUDIO`
//! seam is set and streams); the database opens and takes a write lock (rolled back — nothing is
//! saved); the speech engine gets ready on CUDA; the bundled warm-up WAV transcribes to the words it
//! says (through the text pipeline).

use std::time::{Duration, Instant};

use crate::audio::{self, AudioRecorder, TEST_AUDIO_ENV};
use crate::constants::{SELFTEST_MIC_MS, SIDECAR_READY_TIMEOUT_MS, TRANSCRIBE_TIMEOUT_MS};
use crate::engine::{Engine, EngineConfig, EngineStatus, TranscribeRequest};
use crate::hotkey::HotkeyService;
use crate::paths::Paths;
use crate::pipeline::{self, PipelineInput};
use crate::settings::Settings;
use crate::store::Store;

/// Words the bundled warm-up clip says ("This is the warm-up clip for Wren. It is read once when the
/// speech engine starts, so that the first real dictation is as quick as all the others.").
pub const WARMUP_EXPECT: [&str; 4] = ["clip", "speech", "engine", "dictation"];

type Check = Result<String, String>;

/// Runs every check; returns the process exit code. Prints to the terminal that started Wren (if
/// any) and writes the same lines to `logs\selftest.txt`, so a run with no console (a shortcut, an
/// installer, a launch through WMI) still leaves its result behind.
pub fn run(paths: &Paths, settings: &Settings) -> i32 {
    attach_console();
    let mut out = Output::default();
    out.line(format!("Wren self-test (version {}, {})", env!("CARGO_PKG_VERSION"), crate::BUILD_ID));
    out.line(format!("  exe   {}", std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()));
    let mut failures: Vec<String> = Vec::new();
    let checks: [(&str, fn(&Paths, &Settings, &mut Output) -> Check); 4] = [
        ("hotkey", |_, _, _| check_hotkey()),
        ("microphone", |_, s, _| check_mic(s)),
        ("database", |p, _, _| check_db(p)),
        ("engine", check_engine),
    ];
    for (name, check) in checks {
        match check(paths, settings, &mut out) {
            Ok(detail) => out.line(format!("  ok    {name}: {detail}")),
            Err(why) => {
                out.line(format!("  FAIL  {name}: {why}"));
                failures.push(format!("{name}: {why}"));
            }
        }
    }
    let code = if failures.is_empty() {
        out.line("SELFTEST PASS".into());
        tracing::info!("self-test pass");
        0
    } else {
        out.line(format!("SELFTEST FAIL {}", failures.join("; ")));
        tracing::error!(?failures, "self-test failed");
        1
    };
    let file = paths.logs_dir().join(SELFTEST_FILE);
    if let Err(e) = std::fs::write(&file, out.text) {
        println!("(could not write {}: {e})", file.display());
    }
    code
}

/// File in the logs folder that holds the last self-test's output.
pub const SELFTEST_FILE: &str = "selftest.txt";

/// Lines printed now and kept for `selftest.txt`.
#[derive(Default)]
struct Output {
    text: String,
}

impl Output {
    fn line(&mut self, line: String) {
        println!("{line}");
        self.text.push_str(&line);
        self.text.push_str("\r\n");
    }
}

/// A release build has no console of its own; print into the one that started us, if any.
pub fn attach_console() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn check_hotkey() -> Check {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let svc = HotkeyService::start(tx).map_err(|e| format!("the keyboard hook did not install ({e})"))?;
    drop(svc);
    Ok("keyboard hook installed and removed".into())
}

fn check_mic(settings: &Settings) -> Check {
    let seam = std::env::var_os(TEST_AUDIO_ENV);
    let devices = audio::list_input_devices();
    if seam.is_none() && devices.is_empty() {
        return Err("no microphone found".into());
    }
    let (tx, _rx) = crossbeam_channel::unbounded();
    let rec = AudioRecorder::new(settings.mic_device.clone(), tx).map_err(|e| e.to_string())?;
    let wav = std::env::temp_dir().join(format!("wren-selftest-{}.wav", uuid::Uuid::new_v4()));
    rec.start(wav.clone()).map_err(|e| format!("cannot record ({e})"))?;
    std::thread::sleep(Duration::from_millis(SELFTEST_MIC_MS));
    let recording = rec.stop();
    let _ = std::fs::remove_file(&wav);
    let recording = recording.map_err(|e| format!("cannot finish the recording ({e})"))?;
    if recording.duration_ms == 0 {
        return Err("the microphone delivered no audio".into());
    }
    let source = match seam {
        Some(path) => format!("test seam {}", std::path::Path::new(&path).display()),
        None => format!("{} input device(s)", devices.len()),
    };
    Ok(format!("{source}; recorded {} ms (sound heard: {})", recording.duration_ms, recording.heard_sound))
}

fn check_db(paths: &Paths) -> Check {
    let file = paths.db_file();
    let store = Store::open(&file).map_err(|e| format!("cannot open {} ({e})", file.display()))?;
    drop(store);
    // Take the write lock and roll back: proves the file is writable without saving anything.
    let conn = rusqlite::Connection::open(&file).map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").map_err(|e| format!("not writable ({e})"))?;
    Ok(format!("{} opens, migrates and takes a write lock", file.display()))
}

fn check_engine(paths: &Paths, settings: &Settings, log: &mut Output) -> Check {
    let config = EngineConfig::from_paths(paths, &settings.engine);
    if !config.python.is_file() {
        return Err(format!("the speech runtime is missing ({})", config.python.display()));
    }
    let warmup = config.warmup_wav.clone();
    let (tx, rx) = crossbeam_channel::unbounded();
    let t0 = Instant::now();
    let engine = Engine::start_with(config, tx);
    let deadline = t0 + Duration::from_millis(SIDECAR_READY_TIMEOUT_MS);
    let ready = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(EngineStatus::Ready { model, device, vram_mb }) => break (model, device, vram_mb),
            Ok(EngineStatus::Failed { reason }) | Ok(EngineStatus::NeedsSetup { reason }) => return Err(reason),
            Ok(_) => {}
            Err(_) => return Err(format!("not ready after {} s", SIDECAR_READY_TIMEOUT_MS / 1000)),
        }
    };
    let ready_ms = t0.elapsed().as_millis();
    let (model, device, vram_mb) = ready;
    let t = Instant::now();
    let result = engine
        .transcribe(
            TranscribeRequest { id: "selftest".into(), audio_path: warmup.clone(), bias_terms: vec![], prompt: String::new() },
            Duration::from_millis(TRANSCRIBE_TIMEOUT_MS),
        )
        .map_err(|e| format!("transcribing {} failed ({e})", warmup.display()))?;
    let took = t.elapsed().as_millis();
    engine.shutdown();
    let clip_ms = hound::WavReader::open(&warmup).map(|r| r.duration() as u64 * 1000 / r.spec().sample_rate as u64).unwrap_or(10_000);
    let out = pipeline::run(
        &PipelineInput { raw: &result.text, speech_ms: result.speech_ms, clip_ms, prompt: "", replacements: &[] },
        None,
    );
    let heard: Vec<String> = out
        .final_text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect();
    let missing: Vec<&str> = WARMUP_EXPECT.iter().copied().filter(|w| !heard.iter().any(|h| h == w)).collect();
    // The warm-up clip is a bundled synthetic voice, not the user's speech, so showing it is fine.
    let heard_line = format!("        heard: {:?}", out.final_text);
    log.line(heard_line);
    #[cfg(target_os = "macos")]
    let accelerated = device == "mlx";
    #[cfg(not(target_os = "macos"))]
    let accelerated = device == "cuda";
    if !missing.is_empty() {
        return Err(format!("the warm-up clip did not give the expected words (missing {missing:?}; device {device})"));
    }
    #[cfg(target_os = "macos")]
    let accelerator = "Apple Silicon MLX";
    #[cfg(not(target_os = "macos"))]
    let accelerator = "CUDA";
    if !accelerated {
        // A supported fallback (no usable GPU): it transcribed the clip correctly, so it passes, and
        // says plainly that it is the slower processor path.
        return Ok(format!(
            "{model} ready on {device} (processor fallback, no {accelerator}: slower) in {ready_ms} ms; warm-up clip transcribed in {took} ms"
        ));
    }
    #[cfg(target_os = "macos")]
    let mem_label = "MB RAM";
    #[cfg(not(target_os = "macos"))]
    let mem_label = "MB VRAM";
    Ok(format!("{model} ready on {device} in {ready_ms} ms ({vram_mb} {mem_label}); warm-up clip transcribed in {took} ms"))
}
