//! The meeting's "Me" stream (meetings spec §5.1): the microphone chosen in Settings, opened for the
//! whole meeting (separately from dictation's own stream), converted to 16 kHz mono and stamped with a
//! wall-clock-aligned stream position. If the mic can't be opened, silence keeps the timeline going.

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

pub struct MicSource {
    device: Option<String>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MicSource {
    pub fn new(device: Option<String>) -> Self {
        Self { device, stop: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

impl AudioSource for MicSource {
    fn start(&mut self, tx: Sender<AudioBlock>, _mode: Sender<CaptureMode>) -> anyhow::Result<CaptureMode> {
        let stop = self.stop.clone();
        let device = self.device.clone();
        let thread = std::thread::Builder::new().name("wren-meeting-mic".into()).spawn(move || {
            let started = Instant::now();
            let (raw_tx, raw_rx) = crossbeam_channel::unbounded::<Vec<f32>>();
            let opened = open_mono_input(device.as_deref(), Arc::new(move |m| {
                let _ = raw_tx.send(m);
            }));
            let (rate, _stream) = match opened {
                Ok((rate, stream)) => (rate, Some(stream)),
                Err(e) => {
                    tracing::warn!(error = %e, "meeting microphone unavailable; Me lines will be missing");
                    (RATE as u32, None)
                }
            };
            let mut resampler = match Resampler16k::new(rate) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!(error = %e, "meeting microphone resampler failed");
                    return;
                }
            };
            let mut emitted: u64 = 0;
            let mut out = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(raw) = raw_rx.recv_timeout(Duration::from_millis(100)) {
                    out.clear();
                    if resampler.push(&raw, &mut out).is_ok() && !out.is_empty() {
                        let pos = emitted * 1000 / RATE;
                        emitted += out.len() as u64;
                        if tx.send(AudioBlock { samples: out.clone(), stream_pos_ms: pos }).is_err() {
                            break;
                        }
                    }
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
            let _ = t.join();
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
