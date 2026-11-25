//! The meeting's "Me" stream (meetings spec §5.1): the microphone chosen in Settings, opened for the
//! whole meeting (separately from dictation's own stream), converted to 16 kHz mono and stamped with a
//! wall-clock-aligned stream position. If the mic can't be opened, silence keeps the timeline going; if it
//! stops delivering (headset unplugged, Bluetooth profile switch, default device change) it is reopened.

use super::{AudioBlock, AudioSource};
use crate::audio::{open_mono_input, Resampler16k};
use crate::meetings::CaptureMode;
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RATE: u64 = 16_000;
const PAD_AFTER_MS: u64 = 300;
/// A running mic delivers every ~10 ms; this long without data means it's gone.
const DEAD_AFTER: Duration = Duration::from_secs(2);
/// Wait between reopen attempts.
const REOPEN_EVERY: Duration = Duration::from_secs(2);

pub type MonoCallback = Arc<dyn Fn(Vec<f32>) + Send + Sync>;
/// Opens the input and returns its rate and a handle that keeps it running (dropped to close it).
pub type Opener = Arc<dyn Fn(MonoCallback) -> anyhow::Result<(u32, Box<dyn std::any::Any>)> + Send + Sync>;

pub struct MicSource {
    opener: Opener,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MicSource {
    pub fn new(device: Option<String>) -> Self {
        let opener: Opener = Arc::new(move |cb| {
            let (rate, stream) = open_mono_input(device.as_deref(), cb)?;
            Ok((rate, Box::new(stream) as Box<dyn std::any::Any>))
        });
        Self::with_opener(opener)
    }

    pub fn with_opener(opener: Opener) -> Self {
        Self { opener, stop: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

impl AudioSource for MicSource {
    fn start(&mut self, tx: Sender<AudioBlock>, _mode: Sender<CaptureMode>) -> anyhow::Result<CaptureMode> {
        let stop = self.stop.clone();
        let opener = self.opener.clone();
        let thread = std::thread::Builder::new().name("wren-meeting-mic".into()).spawn(move || {
            let started = Instant::now();
            let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<Vec<f32>>();
            let open = || {
                let tx = raw_tx.clone();
                match opener(Arc::new(move |m| {
                    let _ = tx.send(m);
                })) {
                    Ok((rate, handle)) => match Resampler16k::new(rate) {
                        Ok(r) => Some((handle, r)),
                        Err(e) => {
                            tracing::error!(error = %e, "meeting microphone resampler failed");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "meeting microphone unavailable; retrying");
                        None
                    }
                }
            };
            let mut input = open();
            let mut last_data = Instant::now();
            let mut last_try = Instant::now();
            let mut emitted: u64 = 0;
            let mut out = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(raw) = raw_rx.recv_timeout(Duration::from_millis(100)) {
                    last_data = Instant::now();
                    if let Some((_, resampler)) = input.as_mut() {
                        out.clear();
                        if resampler.push(&raw, &mut out).is_ok() && !out.is_empty() {
                            // After a stall (slow open, reopen), fill the gap with silence first so
                            // this audio keeps its real time instead of landing at the gap's start.
                            let real = started.elapsed().as_millis() as u64;
                            let block_ms = out.len() as u64 * 1000 / RATE;
                            let have = emitted * 1000 / RATE;
                            if real > have + block_ms + PAD_AFTER_MS {
                                let missing = ((real - block_ms - have) * RATE / 1000) as usize;
                                emitted += missing as u64;
                                if tx.send(AudioBlock { samples: vec![0.0; missing], stream_pos_ms: have }).is_err() {
                                    break;
                                }
                            }
                            let pos = emitted * 1000 / RATE;
                            emitted += out.len() as u64;
                            if tx.send(AudioBlock { samples: out.clone(), stream_pos_ms: pos }).is_err() {
                                break;
                            }
                        }
                    }
                }
                // Lost or never opened: reopen (the chosen mic, else the system default).
                let dead = input.is_none() || last_data.elapsed() >= DEAD_AFTER;
                if dead && last_try.elapsed() >= REOPEN_EVERY {
                    last_try = Instant::now();
                    if input.is_some() {
                        tracing::warn!("meeting microphone stopped delivering audio; reopening");
                    }
                    drop(input.take()); // close the old stream first
                    while raw_rx.try_recv().is_ok() {}
                    input = open();
                    last_data = Instant::now();
                }
                let real = started.elapsed().as_millis() as u64;
                let have = emitted * 1000 / RATE;
                if real > have + PAD_AFTER_MS {
                    let missing = ((real - have) * RATE / 1000) as usize;
                    emitted += missing as u64;
                    if tx.send(AudioBlock { samples: vec![0.0; missing], stream_pos_ms: have }).is_err() {
                        break;
                    }
                }
            }
        })?;
        self.thread = Some(thread);
        Ok(CaptureMode::MicOnly)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            // Opening a mic can block inside the OS (a Bluetooth headset waking up): never let that
            // hang Stop or quitting. Wait briefly, then leave the thread to finish on its own.
            let until = Instant::now() + Duration::from_secs(2);
            while !t.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if t.is_finished() {
                let _ = t.join();
            } else {
                tracing::warn!("meeting microphone didn't stop in time; leaving it to finish on its own");
            }
        }
    }
}

impl Drop for MicSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A fake mic at 16 kHz that delivers for `alive` and then goes quiet (the first time only).
    fn fake(calls: Arc<AtomicUsize>, alive: Duration) -> Opener {
        Arc::new(move |cb: MonoCallback| {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            let stop = Arc::new(AtomicBool::new(false));
            let s2 = stop.clone();
            std::thread::spawn(move || {
                let t0 = Instant::now();
                while !s2.load(Ordering::Relaxed) && (n > 0 || t0.elapsed() < alive) {
                    cb(vec![0.3; 160]);
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            struct Handle(Arc<AtomicBool>);
            impl Drop for Handle {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            Ok((16_000, Box::new(Handle(stop)) as Box<dyn std::any::Any>))
        })
    }

    #[test]
    fn a_mic_that_goes_quiet_is_reopened() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _m) = crossbeam_channel::unbounded();
        let mut mic = MicSource::with_opener(fake(calls.clone(), Duration::from_millis(500)));
        mic.start(tx, mtx).unwrap();
        std::thread::sleep(Duration::from_millis(5_500));
        mic.stop();
        assert!(calls.load(Ordering::SeqCst) >= 2, "reopened");
        let blocks: Vec<AudioBlock> = rx.try_iter().collect();
        let late_sound = blocks.iter().filter(|b| b.stream_pos_ms > 4_000).any(|b| b.samples.iter().any(|&x| x > 0.1));
        assert!(late_sound, "audio flows again after the reopen");
        let total: usize = blocks.iter().map(|b| b.samples.len()).sum();
        assert!(total as f64 >= 16_000.0 * 5.0, "timeline stays aligned through the gap ({total})");
    }

    #[test]
    fn audio_after_a_slow_open_keeps_its_real_time() {
        // The mic takes 800 ms to open; its first sound must be stamped ~800 ms, not at 0.
        let opener: Opener = Arc::new(|cb: MonoCallback| {
            std::thread::sleep(Duration::from_millis(800));
            std::thread::spawn(move || {
                for _ in 0..100 {
                    cb(vec![0.3; 160]);
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
            Ok((16_000, Box::new(()) as Box<dyn std::any::Any>))
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _m) = crossbeam_channel::unbounded();
        let mut mic = MicSource::with_opener(opener);
        mic.start(tx, mtx).unwrap();
        std::thread::sleep(Duration::from_millis(1_500));
        mic.stop();
        let first_sound = rx.try_iter().find(|b| b.samples.iter().any(|&x| x > 0.1)).unwrap();
        assert!(first_sound.stream_pos_ms >= 650, "{}", first_sound.stream_pos_ms);
    }

    #[test]
    #[ignore = "live: opens the microphone"]
    fn live_mic_stream_is_wall_clock_aligned() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _m) = crossbeam_channel::unbounded();
        let mut mic = MicSource::new(None);
        mic.start(tx, mtx).unwrap();
        std::thread::sleep(Duration::from_millis(2_000));
        mic.stop();
        let n: usize = rx.try_iter().map(|b| b.samples.len()).sum();
        eprintln!("{n} samples in 2 s");
        assert!(n as f64 >= 16_000.0 * 1.6, "{n}");
    }
}
