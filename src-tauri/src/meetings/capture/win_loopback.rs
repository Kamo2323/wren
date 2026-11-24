//! Windows "Them" capture (meetings spec §5.1, plan Task 5): WASAPI process loopback on the root Teams
//! process and its process tree, so only Teams' sound is recorded and it follows Teams to whichever
//! output device it uses. Fallbacks:
//! - Teams not running → `MicOnly` (silence is still emitted so both streams stay aligned), and Wren
//!   attaches when Teams appears.
//! - Teams running but its capture can't be opened → `AllSound`: process loopback that *excludes* Wren's
//!   own process tree, i.e. everything the computer plays except Wren's sounds.
//!
//! Audio arrives as 16 kHz mono f32 (WASAPI converts). Wall-clock alignment: when no packets arrive (a
//! silent process delivers nothing) zeros are emitted so `stream_pos_ms` tracks real time.

use super::{teams_roots, AudioBlock, AudioSource};
use crate::meetings::CaptureMode;
use crossbeam_channel::Sender;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wasapi::{initialize_mta, AudioCaptureClient, AudioClient, Direction, Handle, SampleType, StreamMode, WaveFormat};

const RATE: u64 = 16_000;
/// How often the loop wakes when nothing arrives, and the size of padding blocks.
const TICK: Duration = Duration::from_millis(100);
/// Pad with silence only when this far behind real time (real packets come every ~10 ms).
const PAD_AFTER_MS: u64 = 300;
/// How often to look for Teams appearing, restarting or quitting.
const RECHECK: Duration = Duration::from_secs(5);

/// Which target to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Teams' root process tree, falling back as described above.
    Teams,
    /// Everything except Wren (used by tests and as the fallback).
    AllSound,
}

/// `(pid, parent pid, exe name)` of every process.
pub fn process_list() -> Vec<(u32, u32, String)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push((e.th32ProcessID, e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

pub fn teams_pid() -> Option<u32> {
    teams_roots(&process_list()).into_iter().next()
}

struct Client {
    _audio: AudioClient,
    capture: AudioCaptureClient,
    event: Handle,
}

fn open_client(pid: u32, include_tree: bool) -> anyhow::Result<Client> {
    let format = WaveFormat::new(32, 32, SampleType::Float, RATE as usize, 1, None);
    let mut audio = AudioClient::new_application_loopback_client(pid, include_tree)
        .map_err(|e| anyhow::anyhow!("process loopback activation failed: {e}"))?;
    let mode = StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
    audio
        .initialize_client(&format, Direction::Capture, mode, None)
        .map_err(|e| anyhow::anyhow!("process loopback init failed: {e}"))?;
    let event = audio.set_get_eventhandle().map_err(|e| anyhow::anyhow!("no event handle: {e}"))?;
    let capture = audio.get_audiocaptureclient().map_err(|e| anyhow::anyhow!("no capture client: {e}"))?;
    audio.start_stream().map_err(|e| anyhow::anyhow!("cannot start loopback: {e}"))?;
    Ok(Client { _audio: audio, capture, event })
}

/// Open the capture for `target`; returns the client (if any), the mode and the Teams pid it follows.
fn connect(target: Target) -> (Option<Client>, CaptureMode, Option<u32>) {
    let all_sound = || match open_client(std::process::id(), false) {
        Ok(c) => (Some(c), CaptureMode::AllSound),
        Err(e) => {
            tracing::warn!(error = %e, "all-sound loopback failed; recording the mic only");
            (None, CaptureMode::MicOnly)
        }
    };
    match target {
        Target::AllSound => {
            let (c, m) = all_sound();
            (c, m, None)
        }
        Target::Teams => match teams_pid() {
            None => (None, CaptureMode::MicOnly, None),
            Some(pid) => match open_client(pid, true) {
                Ok(c) => (Some(c), CaptureMode::TeamsOnly, Some(pid)),
                Err(e) => {
                    tracing::warn!(error = %e, "Teams loopback failed; capturing all computer sound");
                    let (c, m) = all_sound();
                    (c, m, Some(pid))
                }
            },
        },
    }
}

pub struct TeamsLoopback {
    target: Target,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TeamsLoopback {
    pub fn new(target: Target) -> Self {
        Self { target, stop: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

impl AudioSource for TeamsLoopback {
    fn start(&mut self, tx: Sender<AudioBlock>, mode_tx: Sender<CaptureMode>) -> anyhow::Result<CaptureMode> {
        let (first_tx, first_rx) = crossbeam_channel::bounded(1);
        let stop = self.stop.clone();
        let target = self.target;
        let thread = std::thread::Builder::new().name("wren-teams-audio".into()).spawn(move || {
            let _ = initialize_mta().ok();
            let started = Instant::now();
            let (mut client, mut mode, mut pid) = connect(target);
            let _ = first_tx.send(mode);
            let mut emitted: u64 = 0; // samples
            let mut bytes: VecDeque<u8> = VecDeque::new();
            let mut last_check = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                match &client {
                    Some(c) => {
                        let _ = c.event.wait_for_event(TICK.as_millis() as u32);
                        if c.capture.read_from_device_to_deque(&mut bytes).is_err() {
                            tracing::warn!("Teams audio capture stopped; reconnecting");
                            client = None;
                            pid = None;
                        }
                    }
                    None => std::thread::sleep(TICK),
                }
                let n = bytes.len() / 4;
                if n > 0 {
                    let mut samples = Vec::with_capacity(n);
                    for _ in 0..n {
                        let b = [bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap()];
                        samples.push(f32::from_le_bytes(b));
                    }
                    let pos = emitted * 1000 / RATE;
                    emitted += samples.len() as u64;
                    if tx.send(AudioBlock { samples, stream_pos_ms: pos }).is_err() {
                        break;
                    }
                }
                // Keep up with real time when nothing (or too little) arrives.
                let real = started.elapsed().as_millis() as u64;
                let have = emitted * 1000 / RATE;
                if real > have + PAD_AFTER_MS {
                    let missing = ((real - have) * RATE / 1000) as usize;
                    let pos = have;
                    emitted += missing as u64;
                    if tx.send(AudioBlock { samples: vec![0.0; missing], stream_pos_ms: pos }).is_err() {
                        break;
                    }
                }
                if target == Target::Teams && last_check.elapsed() >= RECHECK {
                    last_check = Instant::now();
                    let now_pid = teams_pid();
                    if now_pid != pid || client.is_none() && now_pid.is_some() {
                        let (c, m, p) = connect(Target::Teams);
                        client = c;
                        pid = p;
                        if m != mode {
                            mode = m;
                            let _ = mode_tx.send(m);
                        }
                    }
                }
            }
        })?;
        self.thread = Some(thread);
        let mode = first_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(CaptureMode::MicOnly);
        Ok(mode)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TeamsLoopback {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_wav(path: &std::path::Path, secs: f32) {
        let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for i in 0..(44_100.0 * secs) as usize {
            let v = (i as f32 * 2.0 * std::f32::consts::PI * 1_000.0 / 44_100.0).sin() * 0.3;
            w.write_sample((v * i16::MAX as f32) as i16).unwrap();
        }
        w.finalize().unwrap();
    }

    fn collect(rx: &crossbeam_channel::Receiver<AudioBlock>, for_ms: u64) -> Vec<f32> {
        let end = Instant::now() + Duration::from_millis(for_ms);
        let mut out = Vec::new();
        while Instant::now() < end {
            if let Ok(b) = rx.recv_timeout(Duration::from_millis(50)) {
                out.extend(b.samples);
            }
        }
        out
    }

    #[test]
    #[ignore = "live: plays a tone through the speakers"]
    fn live_default_loopback_hears_tone() {
        let wav = std::env::temp_dir().join(format!("wren-tone-{}.wav", uuid::Uuid::new_v4()));
        tone_wav(&wav, 2.5);
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsLoopback::new(Target::AllSound);
        assert_eq!(src.start(tx, mtx).unwrap(), CaptureMode::AllSound);
        // A process outside our tree must play it (our own tree is excluded by design), so WMI starts it.
        let play = format!("powershell -NoProfile -Command (New-Object Media.SoundPlayer '{}').PlaySync()", wav.display());
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!("Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{{CommandLine=\"{play}\"}} | Out-Null"),
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let samples = collect(&rx, 5_000);
        src.stop();
        let rms = (samples.iter().map(|x| x * x).sum::<f32>() / samples.len().max(1) as f32).sqrt();
        eprintln!("captured {} samples, rms {rms:.4}", samples.len());
        assert!(samples.len() as f32 >= 16_000.0 * 4.5, "wall-clock aligned stream");
        assert!(rms > 0.01, "heard the tone");
    }

    #[test]
    #[ignore = "live: needs Teams running"]
    fn live_teams_loopback_attaches_when_teams_running() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsLoopback::new(Target::Teams);
        let mode = src.start(tx, mtx).unwrap();
        let samples = collect(&rx, 1_500);
        src.stop();
        eprintln!("mode {mode:?}, {} samples", samples.len());
        assert_eq!(mode, CaptureMode::TeamsOnly);
        assert!(samples.len() >= 16_000, "silence keeps the stream aligned");
    }
}
