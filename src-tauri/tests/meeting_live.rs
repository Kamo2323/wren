//! Live meeting round trip (meetings plan Tasks 12–13): the real speech engine and model, real
//! system-audio capture (all computer sound, as when Teams can't be tapped), the real microphone, and
//! optionally the real Claude API with the key in the OS credential store.
//!
//!   WREN_LIVE=1 cargo test --manifest-path src-tauri/Cargo.toml --test meeting_live -- --ignored --nocapture --test-threads=1
//!
//! Plays the tests/fixtures speech WAVs through the speakers from a process outside this one (Wren's own
//! process tree is excluded from capture), then checks the transcribed "Them" lines, the dictation latency
//! while a meeting runs, and (if a key is saved) a Medium enhanced note.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wren_lib::engine::{Engine, EngineConfig, EngineStatus, TranscribeRequest};
use wren_lib::meetings::capture::mic::MicSource;
use wren_lib::meetings::capture::AudioSource;
use wren_lib::meetings::session::{Deps, MeetingService, Transcriber};
use wren_lib::meetings::{Length, Source};
use wren_lib::paths::Paths;
use wren_lib::settings::EngineSettings;
use wren_lib::store::Store;

#[derive(serde::Deserialize)]
struct Fixture {
    file: String,
    expect: Vec<String>,
}

fn fixtures() -> (PathBuf, Vec<Fixture>) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures");
    let m: Vec<Fixture> = serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    let m = m.into_iter().filter(|f| !f.expect.is_empty() && ["weather.wav", "meeting.wav", "madeup.wav"].contains(&f.file.as_str())).collect();
    (dir, m)
}

fn real_local_dir() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap()).join("Wren")
    }
    #[cfg(not(windows))]
    {
        Paths::resolve().local
    }
}

struct EngineT(Arc<Engine>);
impl Transcriber for EngineT {
    fn transcribe(&self, wav: &Path, clip_ms: u64, bias: &[String]) -> Result<String, String> {
        let req = TranscribeRequest { id: uuid::Uuid::new_v4().to_string(), audio_path: wav.into(), bias_terms: bias.to_vec(), prompt: String::new() };
        let r = self.0.transcribe(req, Duration::from_secs(60)).map_err(|e| e.to_string())?;
        let g = wren_lib::pipeline::guards::apply(&r.text, r.speech_ms, clip_ms, "");
        Ok(wren_lib::pipeline::tidy::normalise_whitespace(&g.text))
    }
}

/// Play a WAV from a process outside this one's tree.
fn play_detached(wav: &Path) {
    #[cfg(windows)]
    {
        let play = format!("powershell -NoProfile -Command (New-Object Media.SoundPlayer '{}').PlaySync()", wav.display());
        let ok = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &format!("Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{CommandLine=\"{play}\"}} | Out-Null")])
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
    #[cfg(target_os = "macos")]
    {
        // `launchctl asuser` isn't needed: a double fork via `nohup sh -c … &` leaves our tree.
        let ok = std::process::Command::new("sh")
            .args(["-c", &format!("nohup afplay '{}' >/dev/null 2>&1 &", wav.display())])
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }
}

fn wav_secs(p: &Path) -> f64 {
    let r = hound::WavReader::open(p).unwrap();
    r.duration() as f64 / r.spec().sample_rate as f64
}

fn p50(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    v[v.len() / 2]
}

fn words(s: &str) -> String {
    s.to_lowercase()
}

#[test]
#[ignore = "live: speakers, mic, GPU/Metal, network; run with WREN_LIVE=1 and --ignored"]
fn meeting_round_trip_on_the_real_engine() {
    if std::env::var("WREN_LIVE").as_deref() != Ok("1") {
        eprintln!("skipped: set WREN_LIVE=1");
        return;
    }
    let (dir, fx) = fixtures();
    let tmp = std::env::temp_dir().join(format!("wren-meeting-live-{}", uuid::Uuid::new_v4()));
    let paths = Paths { roaming: tmp.join("roaming"), local: real_local_dir() };
    let mut cfg = EngineConfig::from_paths(&paths, &EngineSettings::default());
    cfg.log_file = tmp.join("logs").join("sidecar.log");
    let (tx, rx) = crossbeam_channel::unbounded();
    let engine = Arc::new(Engine::start_with(cfg, tx));
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(EngineStatus::Ready { model, device, .. }) => {
                eprintln!("engine ready: {model} on {device}");
                break;
            }
            Ok(EngineStatus::Failed { reason }) => panic!("engine failed: {reason}"),
            _ => assert!(Instant::now() < deadline, "engine not ready in 90 s"),
        }
    }
    let probe = dir.join("weather.wav");
    let dictation = |label: &str| {
        let mut v = Vec::new();
        for _ in 0..6 {
            let t = Instant::now();
            let req = TranscribeRequest { id: uuid::Uuid::new_v4().to_string(), audio_path: probe.clone(), bias_terms: vec![], prompt: String::new() };
            engine.transcribe(req, Duration::from_secs(60)).expect("dictation transcribes");
            v.push(t.elapsed().as_millis() as u64);
            std::thread::sleep(Duration::from_millis(400));
        }
        let m = p50(v.clone());
        eprintln!("dictation latency {label}: p50 {m} ms {v:?}");
        m
    };
    let idle_p50 = dictation("without a meeting");

    let store = Arc::new(Store::open(&tmp.join("wren.db")).unwrap());
    let deps = Deps {
        store: store.clone(),
        transcriber: Arc::new(EngineT(engine.clone())),
        meetings_dir: tmp.join("meetings"),
        make_sources: Arc::new(|| {
            #[cfg(windows)]
            let them: Box<dyn AudioSource> = Box::new(wren_lib::meetings::capture::win_loopback::TeamsLoopback::new(
                wren_lib::meetings::capture::win_loopback::Target::AllSound,
            ));
            #[cfg(target_os = "macos")]
            let them: Box<dyn AudioSource> = Box::new(wren_lib::meetings::capture::mac_tap::TeamsTap::new(
                wren_lib::meetings::capture::mac_tap::Target::AllSound,
            ));
            (Box::new(MicSource::new(None)) as Box<dyn AudioSource>, them)
        }),
        make_teams_reader: None,
        bias_terms: Arc::new(Vec::new),
        me_name: Arc::new(|| "Maximilian".into()),
        drain_timeout: Duration::from_secs(30),
    };
    let svc = MeetingService::new(deps);
    let m = svc.start(Some("Live test".into())).unwrap();
    eprintln!("capture mode {:?}", svc.active().unwrap().capture_mode);
    std::thread::sleep(Duration::from_millis(800));

    // Play the clips one after another; measure dictation latency while the meeting works.
    let mut played = 0.0;
    for f in &fx {
        let p = dir.join(&f.file);
        played += wav_secs(&p);
        play_detached(&p);
        std::thread::sleep(Duration::from_secs_f64(wav_secs(&p) + 1.6));
    }
    let busy_p50 = dictation("with a meeting running");
    std::thread::sleep(Duration::from_secs(3));
    let t_stop = Instant::now();
    svc.stop();
    eprintln!("stop took {} ms; played {played:.1} s of speech", t_stop.elapsed().as_millis());

    let segs = store.segments(&m.id).unwrap();
    let them: Vec<_> = segs.iter().filter(|s| s.source == Source::Them).collect();
    let me: Vec<_> = segs.iter().filter(|s| s.source == Source::Me).collect();
    for s in &segs {
        eprintln!("[{:>6} ms] {:?} {:?}: {}", s.start_ms, s.source, s.speaker, s.text);
    }
    let all_them = words(&them.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "));
    let mut heard = 0;
    for f in &fx {
        let hit = f.expect.iter().filter(|w| all_them.contains(&w.to_lowercase())).count();
        eprintln!("{}: {hit}/{} expected words in Them lines", f.file, f.expect.len());
        if hit * 3 >= f.expect.len() * 2 {
            heard += 1;
        }
    }
    eprintln!("{} Them lines, {} Me lines; {heard}/{} clips heard", them.len(), me.len(), fx.len());
    assert_eq!(heard, fx.len(), "every clip transcribed from system audio");
    assert!(!segs.iter().any(|s| s.text == "[not transcribed]"), "no clip left untranscribed");
    let row = store.meeting_get(&m.id).unwrap().unwrap();
    assert_eq!(row.status, wren_lib::meetings::MeetingStatus::Ended);
    let me_wav = tmp.join("meetings").join(&m.id).join("me.wav");
    let them_wav = tmp.join("meetings").join(&m.id).join("them.wav");
    let (a, b) = (wav_secs(&me_wav), wav_secs(&them_wav));
    eprintln!("me.wav {a:.1} s, them.wav {b:.1} s");
    assert!((a - b).abs() < 1.5, "both recordings share the timeline");
    eprintln!("dictation p50 idle {idle_p50} ms → busy {busy_p50} ms");
    assert!(busy_p50 <= idle_p50 + 2_500, "dictation waits behind at most one meeting clip");

    // Enhanced notes with the saved key (skipped without one).
    if wren_lib::meetings::claude::KeyStore::system().get().is_some() {
        let t = Instant::now();
        let mut deltas = 0;
        let note = wren_lib::meetings::enhance::generate(
            &store,
            &wren_lib::meetings::enhance::ClaudeWriter,
            "claude-opus-5-5",
            "Maximilian",
            &m.id,
            Length::Medium,
            &mut |e| {
                if matches!(e, wren_lib::meetings::enhance::NoteEvent::Delta { .. }) {
                    deltas += 1;
                }
            },
        )
        .expect("Claude writes the notes");
        eprintln!(
            "Medium note in {} ms, {deltas} deltas, {} in / {} out / {} cache-write tokens, ~${:.4}\n{}",
            t.elapsed().as_millis(),
            note.input_tokens,
            note.output_tokens,
            note.cache_write_tokens,
            note.cost_usd,
            note.markdown
        );
        assert!(note.markdown.contains("## Summary"));
        assert!(deltas > 1, "streamed");
    } else {
        eprintln!("no Claude key saved: enhanced-notes step skipped");
    }
    drop(svc);
    engine.shutdown();
    let _ = std::fs::remove_dir_all(&tmp);
}
