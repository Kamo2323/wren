//! Microphone capture (spec §4): cpal on WASAPI → 16 kHz mono → 16-bit PCM WAV (hound), plus level
//! events (RMS → 0..1 on a dB curve) at `LEVEL_EVENTS_HZ` for the popup.
//!
//! The stream is built and started on `start` (chord-down) and dropped on `stop`, so the mic light is
//! only on while Wren is listening. With no device named, every `start` opens the *current* Windows
//! default input, so a change of default between dictations is followed (cpal also re-routes a
//! running default stream).
//!
//! Threads: the cpal callback only down-mixes to mono and hands the block to a writer thread, which
//! resamples (rubato), writes the WAV and measures levels. Nothing heavy runs on the audio thread.
//!
//! Test seam: if `WREN_TEST_AUDIO` names a WAV file when the recorder is created, every recording
//! streams that file in real time instead of the mic (levels included), then goes quiet.
//!
//! The streaming resampler is adapted from Handy (src-tauri/src/audio_toolkit/audio/decode.rs),
//! MIT, Copyright (c) 2025 CJ Pais.

use crate::constants::{
    AUDIO_SILENCE_DBFS, ENGINE_SAMPLE_RATE, LEVEL_CEIL_DBFS, LEVEL_EVENTS_HZ, LEVEL_FLOOR_DBFS,
    TEST_AUDIO_CHUNK_MS,
};
use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Environment variable naming a WAV that replaces the microphone (spec §4 test seam).
pub const TEST_AUDIO_ENV: &str = "WREN_TEST_AUDIO";

/// Frames the resampler takes per step.
const RESAMPLER_CHUNK_FRAMES: usize = 1024;
/// `last_sound_ms` before any sound arrived.
const NEVER: u64 = u64::MAX;

#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub wav_path: PathBuf,
    pub duration_ms: u64,
    pub heard_sound: bool,
}

/// Names of the input devices Windows offers (for the Settings picker).
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    match host.input_devices() {
        Ok(devices) => devices.filter_map(|d| device_name(&d)).collect(),
        Err(err) => {
            tracing::warn!(error = %err, "cannot list microphones");
            Vec::new()
        }
    }
}

fn device_name(device: &cpal::Device) -> Option<String> {
    device.description().ok().map(|d| d.name().to_owned())
}

fn find_input_device(name: &str) -> Option<cpal::Device> {
    cpal::default_host()
        .input_devices()
        .ok()?
        .find(|d| device_name(d).as_deref() == Some(name))
}

/// RMS (0..1 full scale) → meter level 0..1, linear in dB between the floor and ceiling constants.
fn level_from_rms(rms: f32) -> f32 {
    // Also catches NaN.
    if rms.is_nan() || rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db - LEVEL_FLOOR_DBFS) / (LEVEL_CEIL_DBFS - LEVEL_FLOOR_DBFS)).clamp(0.0, 1.0)
}

fn silence_rms() -> f32 {
    10f32.powf(AUDIO_SILENCE_DBFS / 20.0)
}

// ------------------------------------------------------------------------------------- recorder

enum Chunk {
    /// Mono samples at the source rate.
    Samples(Vec<f32>),
    End,
}

/// What the writer thread shares with the recorder while a recording runs.
struct Clock {
    started: Instant,
    /// Clock ms of the last level window above the silence floor, or `NEVER`.
    last_sound_ms: AtomicU64,
    heard: AtomicBool,
}

impl Clock {
    fn new() -> Self {
        Self { started: Instant::now(), last_sound_ms: AtomicU64::new(NEVER), heard: AtomicBool::new(false) }
    }
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }
}

struct Feeder {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

struct Active {
    stream: Option<cpal::Stream>,
    feeder: Option<Feeder>,
    tx: Sender<Chunk>,
    writer: JoinHandle<Result<u64>>,
    wav_path: PathBuf,
    clock: Arc<Clock>,
}

pub struct AudioRecorder {
    /// `None` follows the Windows default input.
    device: Mutex<Option<String>>,
    level_tx: Sender<f32>,
    test_audio: Option<PathBuf>,
    active: Mutex<Option<Active>>,
}

impl AudioRecorder {
    /// `device` is a name from `list_input_devices`, or `None` for the Windows default. A named device
    /// that is not plugged in is remembered; recordings use the default until it comes back.
    pub fn new(device: Option<String>, level_tx: Sender<f32>) -> Result<Self> {
        let test_audio = std::env::var_os(TEST_AUDIO_ENV).map(PathBuf::from);
        if let Some(path) = &test_audio {
            tracing::warn!(
                path = %path.display(),
                "WREN_TEST_AUDIO is set: the microphone is replaced by this file (test seam, never for normal use)"
            );
            if !path.is_file() {
                bail!("WREN_TEST_AUDIO names a file that does not exist: {}", path.display());
            }
        } else if let Some(name) = &device
            && find_input_device(name).is_none()
        {
            tracing::warn!("chosen microphone is not connected; using the Windows default until it is");
        }
        if test_audio.is_none() {
            warm_up_input(device.as_deref());
        }
        Ok(Self::with_test_audio(device, level_tx, test_audio))
    }

    fn with_test_audio(device: Option<String>, level_tx: Sender<f32>, test_audio: Option<PathBuf>) -> Self {
        Self { device: Mutex::new(device), level_tx, test_audio, active: Mutex::new(None) }
    }

    /// Start capturing into `wav_path` (the stream is opened here). One recording at a time.
    ///
    /// The device is opened *before* the recorder's lock is taken (a cold WASAPI open can take over a
    /// second), so `silence_ms`/`elapsed_ms`/`stop` callers are never blocked behind it.
    pub fn start(&self, wav_path: PathBuf) -> Result<()> {
        if self.active.lock().is_some() {
            bail!("already recording");
        }
        if let Some(dir) = wav_path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        let clock = Arc::new(Clock::new());
        let (tx, rx) = crossbeam_channel::unbounded::<Chunk>();
        // Blocks are buffered in the channel until the writer thread starts below.
        let (rate, stream, feeder) = match &self.test_audio {
            Some(path) => start_feeder(path, tx.clone()).map(|(rate, feeder)| (rate, None, Some(feeder)))?,
            None => {
                let name = self.device.lock().clone();
                open_stream(name.as_deref(), tx.clone()).map(|(rate, stream)| (rate, Some(stream), None))?
            }
        };
        let stop_source = |stream: Option<cpal::Stream>, feeder: Option<Feeder>| {
            drop(stream);
            if let Some(f) = feeder {
                f.stop.store(true, Ordering::Relaxed);
                let _ = f.handle.join();
            }
        };

        let mut active = self.active.lock();
        if active.is_some() {
            // Another start() won the race while the device was opening.
            drop(active);
            stop_source(stream, feeder);
            bail!("already recording");
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: ENGINE_SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let wav = match hound::WavWriter::create(&wav_path, spec) {
            Ok(w) => w,
            Err(err) => {
                drop(active);
                stop_source(stream, feeder);
                return Err(anyhow!("cannot create {}: {err}", wav_path.display()));
            }
        };
        let writer = {
            let clock = clock.clone();
            let level_tx = self.level_tx.clone();
            match std::thread::Builder::new()
                .name("wren-audio-writer".into())
                .spawn(move || run_writer(rx, rate, wav, &clock, &level_tx))
            {
                Ok(h) => h,
                Err(err) => {
                    drop(active);
                    stop_source(stream, feeder);
                    let _ = std::fs::remove_file(&wav_path);
                    return Err(anyhow!("cannot start the audio writer thread: {err}"));
                }
            }
        };
        *active = Some(Active { stream, feeder, tx, writer, wav_path, clock });
        Ok(())
    }

    /// Stop capturing and finish the WAV.
    pub fn stop(&self) -> Result<Recording> {
        let active = self.active.lock().take().ok_or_else(|| anyhow!("not recording"))?;
        let heard = active.clock.heard.load(Ordering::Relaxed);
        let wav_path = active.wav_path.clone();
        let samples = finish(active)?;
        Ok(Recording {
            wav_path,
            duration_ms: samples * 1000 / ENGINE_SAMPLE_RATE as u64,
            heard_sound: heard,
        })
    }

    /// Stop capturing and delete the WAV. Never fails; no-op when not recording.
    pub fn cancel(&self) {
        if let Some(active) = self.active.lock().take() {
            let path = active.wav_path.clone();
            if let Err(err) = finish(active) {
                tracing::warn!(error = %err, "recording cancel: writer failed");
            }
            let _ = std::fs::remove_file(path);
        }
    }

    /// Continuous silence so far in this recording (ms), for the hands-free auto-stop. 0 when idle.
    pub fn silence_ms(&self) -> u64 {
        match &*self.active.lock() {
            Some(a) => {
                let now = a.clock.now_ms();
                match a.clock.last_sound_ms.load(Ordering::Relaxed) {
                    NEVER => now,
                    last => now.saturating_sub(last),
                }
            }
            None => 0,
        }
    }

    /// Time since `start` (ms). 0 when idle.
    pub fn elapsed_ms(&self) -> u64 {
        self.active.lock().as_ref().map(|a| a.clock.now_ms()).unwrap_or(0)
    }

    /// Choose the microphone for the next recording (`None` = Windows default). A name that is not
    /// connected right now is refused, so the picker can say so.
    pub fn set_device(&self, device: Option<String>) -> Result<()> {
        if let Some(name) = &device
            && find_input_device(name).is_none()
        {
            bail!("no microphone called \"{name}\" is connected");
        }
        *self.device.lock() = device;
        Ok(())
    }
}

impl Drop for AudioRecorder {
    fn drop(&mut self) {
        if let Some(active) = self.active.get_mut().take() {
            let _ = finish(active);
        }
    }
}

/// Stop the source, flush the writer, finalise the WAV. Returns 16 kHz samples written.
fn finish(active: Active) -> Result<u64> {
    let Active { stream, feeder, tx, writer, .. } = active;
    drop(stream); // no more callbacks
    if let Some(f) = feeder {
        f.stop.store(true, Ordering::Relaxed);
        let _ = f.handle.join();
    }
    let _ = tx.send(Chunk::End);
    drop(tx);
    writer.join().map_err(|_| anyhow!("audio writer thread panicked"))?
}

/// Open the device once at startup (WASAPI and COM set-up cost a few hundred ms the first time), so
/// the stream on chord-down starts in tens of ms and the first word is not clipped. No stream is opened.
fn warm_up_input(name: Option<&str>) {
    let t0 = Instant::now();
    let device = name.and_then(find_input_device).or_else(|| cpal::default_host().default_input_device());
    match device.map(|d| d.default_input_config()) {
        Some(Ok(cfg)) => tracing::info!(rate = cfg.sample_rate(), channels = cfg.channels(), ms = t0.elapsed().as_millis() as u64, "microphone ready"),
        Some(Err(err)) => tracing::warn!(error = %err, "microphone has no usable format"),
        None => tracing::warn!("no microphone found"),
    }
}

fn open_stream(name: Option<&str>, tx: Sender<Chunk>) -> Result<(u32, cpal::Stream)> {
    let host = cpal::default_host();
    let device = match name {
        Some(n) => match find_input_device(n) {
            Some(d) => d,
            None => {
                tracing::warn!("chosen microphone is not connected; recording from the Windows default");
                host.default_input_device().ok_or_else(|| anyhow!("no microphone found"))?
            }
        },
        None => host.default_input_device().ok_or_else(|| anyhow!("no microphone found"))?,
    };
    let supported = device.default_input_config().context("microphone has no usable format")?;
    let rate = supported.sample_rate();
    let channels = (supported.channels() as usize).max(1);
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let on_error = |err: cpal::Error| tracing::warn!(error = %err, "microphone stream error");

    macro_rules! build {
        ($t:ty) => {{
            let tx = tx.clone();
            device.build_input_stream::<$t, _, _>(
                config.clone(),
                move |data: &[$t], _: &cpal::InputCallbackInfo| send_mono(data, channels, &tx),
                on_error,
                None,
            )
        }};
    }
    let stream = match format {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::F64 => build!(f64),
        other => bail!("microphone sample format {other:?} is not supported"),
    }
    .context("cannot open the microphone")?;
    stream.play().context("cannot start the microphone")?;
    tracing::info!(rate, channels, format = ?format, "microphone stream started");
    Ok((rate, stream))
}

fn send_mono<T>(data: &[T], channels: usize, tx: &Sender<Chunk>)
where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    let mono: Vec<f32> = data
        .chunks_exact(channels)
        .map(|frame| frame.iter().map(|&s| <f32 as cpal::FromSample<T>>::from_sample_(s)).sum::<f32>() / channels as f32)
        .collect();
    let _ = tx.send(Chunk::Samples(mono));
}

/// The seam: decode the whole file (they are short), then send it in `TEST_AUDIO_CHUNK_MS` blocks,
/// each when it would have been captured.
fn start_feeder(path: &Path, tx: Sender<Chunk>) -> Result<(u32, Feeder)> {
    let (rate, samples) = read_wav_mono(path)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let handle = std::thread::Builder::new()
        .name("wren-test-audio".into())
        .spawn(move || {
            let chunk = ((rate as u64 * TEST_AUDIO_CHUNK_MS / 1000) as usize).max(1);
            let started = Instant::now();
            for (i, block) in samples.chunks(chunk).enumerate() {
                let due = started + Duration::from_millis(TEST_AUDIO_CHUNK_MS * (i as u64 + 1));
                while Instant::now() < due {
                    if stop2.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep((due - Instant::now()).min(Duration::from_millis(TEST_AUDIO_CHUNK_MS)));
                }
                if stop2.load(Ordering::Relaxed) || tx.send(Chunk::Samples(block.to_vec())).is_err() {
                    return;
                }
            }
        })
        .context("cannot start the test-audio thread")?;
    Ok((rate, Feeder { stop, handle }))
}

/// Any PCM or float WAV → (rate, mono f32).
fn read_wav_mono(path: &Path) -> Result<(u32, Vec<f32>)> {
    let mut reader = hound::WavReader::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let spec = reader.spec();
    let channels = (spec.channels as usize).max(1);
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let mono = interleaved.chunks_exact(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect();
    Ok((spec.sample_rate, mono))
}

// ------------------------------------------------------------------------------------- writer

fn run_writer(
    rx: Receiver<Chunk>,
    rate: u32,
    wav: hound::WavWriter<BufWriter<File>>,
    clock: &Clock,
    level_tx: &Sender<f32>,
) -> Result<u64> {
    let mut resampler = Resampler16k::new(rate)?;
    let mut sink = Sink::new(wav, clock, level_tx);
    let mut out = Vec::new();
    for chunk in rx.iter() {
        match chunk {
            Chunk::Samples(samples) => {
                resampler.push(&samples, &mut out)?;
                sink.push(&out);
                out.clear();
            }
            Chunk::End => break,
        }
    }
    resampler.finish(&mut out)?;
    sink.push(&out);
    sink.finish()
}

/// 16 kHz samples → WAV + level windows.
struct Sink<'a> {
    wav: hound::WavWriter<BufWriter<File>>,
    error: Option<hound::Error>,
    written: u64,
    window: usize,
    sum_sq: f64,
    count: usize,
    silence: f32,
    clock: &'a Clock,
    level_tx: &'a Sender<f32>,
}

impl<'a> Sink<'a> {
    fn new(wav: hound::WavWriter<BufWriter<File>>, clock: &'a Clock, level_tx: &'a Sender<f32>) -> Self {
        Self {
            wav,
            error: None,
            written: 0,
            window: (ENGINE_SAMPLE_RATE / LEVEL_EVENTS_HZ) as usize,
            sum_sq: 0.0,
            count: 0,
            silence: silence_rms(),
            clock,
            level_tx,
        }
    }

    fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            let s = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
            if self.error.is_none() {
                match self.wav.write_sample((s * i16::MAX as f32).round() as i16) {
                    Ok(()) => self.written += 1,
                    Err(e) => self.error = Some(e),
                }
            }
            self.sum_sq += (s as f64) * (s as f64);
            self.count += 1;
            if self.count == self.window {
                let rms = (self.sum_sq / self.count as f64).sqrt() as f32;
                if rms >= self.silence {
                    self.clock.last_sound_ms.store(self.clock.now_ms(), Ordering::Relaxed);
                    self.clock.heard.store(true, Ordering::Relaxed);
                }
                let _ = self.level_tx.try_send(level_from_rms(rms));
                self.sum_sq = 0.0;
                self.count = 0;
            }
        }
    }

    fn finish(self) -> Result<u64> {
        if let Some(e) = self.error {
            return Err(anyhow!("writing the recording failed: {e}"));
        }
        self.wav.finalize().context("finishing the recording failed")?;
        Ok(self.written)
    }
}

/// Mono audio at the source rate in, 16 kHz out, one resampler step at a time.
struct Resampler16k {
    input_len: usize,
    /// `None` when the source is already 16 kHz.
    stage: Option<Stage>,
}

struct Stage {
    resampler: Fft<f32>,
    /// Input not yet resampled.
    pending: Vec<f32>,
    /// One step's output.
    scratch: Vec<f32>,
    /// Output frames of the resampler's delay still to drop.
    delay_left: usize,
    produced: usize,
}

impl Resampler16k {
    fn new(rate: u32) -> Result<Self> {
        let target = ENGINE_SAMPLE_RATE as usize;
        let stage = if rate as usize == target {
            None
        } else {
            let resampler = Fft::<f32>::new(rate as usize, target, RESAMPLER_CHUNK_FRAMES, 1, FixedSync::Input)
                .map_err(|e| anyhow!("cannot resample from {rate} Hz: {e}"))?;
            Some(Stage {
                pending: Vec::with_capacity(2 * resampler.input_frames_max()),
                scratch: vec![0.0; resampler.output_frames_max()],
                delay_left: resampler.output_delay(),
                produced: 0,
                resampler,
            })
        };
        Ok(Self { input_len: 0, stage })
    }

    fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<()> {
        self.input_len += samples.len();
        let Some(stage) = self.stage.as_mut() else {
            out.extend_from_slice(samples);
            return Ok(());
        };
        stage.pending.extend_from_slice(samples);
        let mut taken = 0;
        while stage.pending.len() - taken >= stage.resampler.input_frames_next() {
            taken += stage.step(taken, None, out)?;
        }
        stage.pending.drain(..taken);
        Ok(())
    }

    /// Flush the tail so the output is exactly as long as the input (at 16 kHz).
    fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        let Some(stage) = self.stage.as_mut() else {
            return Ok(());
        };
        let expected = (stage.resampler.resample_ratio() * self.input_len as f64).round() as usize;
        let left = stage.pending.len();
        if left > 0 {
            stage.step(0, Some(left), out)?;
            stage.pending.clear();
        }
        // The resampler holds back its delay's worth; silence pushes it out.
        while stage.produced < expected {
            stage.step(0, Some(0), out)?;
        }
        let extra = stage.produced - expected;
        out.truncate(out.len().saturating_sub(extra));
        stage.produced = expected;
        Ok(())
    }
}

impl Stage {
    /// Resample one step of `pending` from `offset` (or only `partial` of it, padded with silence)
    /// into `out`. Returns the frames taken.
    fn step(&mut self, offset: usize, partial: Option<usize>, out: &mut Vec<f32>) -> Result<usize> {
        let input = InterleavedSlice::new(&self.pending[..], 1, self.pending.len())
            .map_err(|e| anyhow!("resample failed: {e}"))?;
        let scratch_len = self.scratch.len();
        let mut output = InterleavedSlice::new_mut(&mut self.scratch[..], 1, scratch_len)
            .map_err(|e| anyhow!("resample failed: {e}"))?;
        let mut indexing = Indexing::new().input_offset(offset);
        indexing.partial_len = partial;
        let (taken, produced) = self
            .resampler
            .process_into_buffer(&input, &mut output, Some(&indexing))
            .map_err(|e| anyhow!("resample failed: {e}"))?;
        let dropped = self.delay_left.min(produced);
        self.delay_left -= dropped;
        out.extend_from_slice(&self.scratch[dropped..produced]);
        self.produced += produced - dropped;
        Ok(taken)
    }
}

/// Whole-buffer resample (tests).
#[cfg(test)]
fn resample_to_16k(input: &[f32], rate: u32) -> Vec<f32> {
    let mut r = Resampler16k::new(rate).unwrap();
    let mut out = Vec::new();
    r.push(input, &mut out).unwrap();
    r.finish(&mut out).unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{ENGINE_SAMPLE_RATE, LEVEL_EVENTS_HZ};
    use std::time::{Duration, Instant};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wren-audio-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A WAV at `rate` with `channels`, `secs` long: a 440 Hz tone at `amp` (0 = digital silence).
    fn write_tone(path: &std::path::Path, rate: u32, channels: u16, secs: f32, amp: f32) {
        let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        let n = (rate as f32 * secs) as usize;
        for i in 0..n {
            let v = (amp * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() * i16::MAX as f32) as i16;
            for _ in 0..channels {
                w.write_sample(v).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    fn wav_ms(path: &std::path::Path) -> (u64, hound::WavSpec) {
        let r = hound::WavReader::open(path).unwrap();
        let spec = r.spec();
        (r.duration() as u64 * 1000 / spec.sample_rate as u64, spec)
    }

    #[test]
    fn level_curve_maps_dbfs_to_0_1() {
        assert_eq!(level_from_rms(0.0), 0.0);
        assert_eq!(level_from_rms(1.0), 1.0);
        let quiet = level_from_rms(10f32.powf(-55.0 / 20.0));
        let loud = level_from_rms(10f32.powf(-20.0 / 20.0));
        assert!(quiet > 0.0 && quiet < 0.2, "{quiet}");
        assert!(loud > 0.7 && loud < 1.0, "{loud}");
    }

    #[test]
    fn resampler_keeps_duration_and_pitch() {
        let rate = 48_000;
        let input: Vec<f32> = (0..rate).map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin() * 0.5).collect();
        let out = resample_to_16k(&input, rate);
        assert_eq!(out.len(), ENGINE_SAMPLE_RATE as usize);
        // 440 Hz → ~880 zero crossings per second, whatever the rate.
        let crossings = out.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        assert!((870..=890).contains(&crossings), "{crossings}");
        assert_eq!(resample_to_16k(&input[..16_000], 16_000).len(), 16_000, "16 kHz passes through");
    }

    #[test]
    fn test_audio_seam_streams_file_to_16k_wav_in_real_time() {
        let dir = temp_dir("seam");
        let src = dir.join("in.wav");
        write_tone(&src, 44_100, 2, 1.5, 0.3);
        let (tx, rx) = crossbeam_channel::unbounded();
        let rec = AudioRecorder::with_test_audio(None, tx, Some(src));
        let out = dir.join("out.wav");
        let t0 = Instant::now();
        rec.start(out.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(1_700));
        assert!(rec.elapsed_ms() >= 1_600);
        let r = rec.stop().unwrap();
        assert!(t0.elapsed() >= Duration::from_millis(1_500), "real time, not instant");
        assert_eq!(r.wav_path, out);
        assert!(r.heard_sound);
        assert!(r.duration_ms.abs_diff(1_500) <= 50, "duration {}", r.duration_ms);
        let (ms, spec) = wav_ms(&out);
        assert!(ms.abs_diff(1_500) <= 50, "wav {ms} ms");
        assert_eq!((spec.channels, spec.sample_rate, spec.bits_per_sample), (1, ENGINE_SAMPLE_RATE, 16));
        // ~LEVEL_EVENTS_HZ level events per second of audio, all in 0..1, loud for a 0.3 tone.
        let levels: Vec<f32> = rx.try_iter().collect();
        let expect = (LEVEL_EVENTS_HZ as f32 * 1.5) as usize;
        assert!(levels.len().abs_diff(expect) <= 2, "{} levels", levels.len());
        assert!(levels.iter().all(|l| (0.0..=1.0).contains(l)));
        assert!(levels.iter().filter(|&&l| l > 0.5).count() > expect / 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn silent_seam_file_hears_nothing_and_counts_silence() {
        let dir = temp_dir("silent");
        let src = dir.join("in.wav");
        write_tone(&src, 16_000, 1, 1.0, 0.0);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let rec = AudioRecorder::with_test_audio(None, tx, Some(src));
        rec.start(dir.join("out.wav")).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        let s = rec.silence_ms();
        assert!(s >= 500, "silence {s}");
        let r = rec.stop().unwrap();
        assert!(!r.heard_sound);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sound_resets_silence_counter() {
        let dir = temp_dir("loud");
        let src = dir.join("in.wav");
        write_tone(&src, 16_000, 1, 2.0, 0.3);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let rec = AudioRecorder::with_test_audio(None, tx, Some(src));
        rec.start(dir.join("out.wav")).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        assert!(rec.silence_ms() < 200, "silence {}", rec.silence_ms());
        rec.cancel();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancel_deletes_the_wav_and_stop_without_start_errors() {
        let dir = temp_dir("cancel");
        let src = dir.join("in.wav");
        write_tone(&src, 16_000, 1, 1.0, 0.3);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let rec = AudioRecorder::with_test_audio(None, tx, Some(src));
        assert!(rec.stop().is_err());
        let out = dir.join("out.wav");
        rec.start(out.clone()).unwrap();
        assert!(rec.start(dir.join("again.wav")).is_err(), "one recording at a time");
        std::thread::sleep(Duration::from_millis(200));
        rec.cancel();
        assert!(!out.exists());
        assert_eq!(rec.elapsed_ms(), 0);
        // Usable again after a cancel.
        rec.start(out.clone()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.stop().is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The real microphone (WREN_LIVE=1): one second from the Windows default input.
    #[test]
    #[ignore = "needs a microphone; run with WREN_LIVE=1 and --ignored"]
    fn live_default_mic_records_a_second() {
        if std::env::var("WREN_LIVE").as_deref() != Ok("1") {
            return;
        }
        println!("inputs: {:?}", list_input_devices());
        let dir = temp_dir("live");
        let (tx, rx) = crossbeam_channel::unbounded();
        let tw = Instant::now();
        warm_up_input(None);
        println!("warm-up {} ms", tw.elapsed().as_millis());
        let rec = AudioRecorder::with_test_audio(None, tx, None);
        let t0 = Instant::now();
        rec.start(dir.join("mic.wav")).unwrap();
        println!("stream open in {} ms", t0.elapsed().as_millis());
        std::thread::sleep(Duration::from_millis(1_000));
        let r = rec.stop().unwrap();
        let peak = hound::WavReader::open(&r.wav_path).unwrap().samples::<i16>().map(|s| s.unwrap().unsigned_abs()).max().unwrap_or(0);
        println!("peak sample {peak} of 32767");
        let t1 = Instant::now();
        rec.start(dir.join("mic2.wav")).unwrap();
        println!("second stream open in {} ms", t1.elapsed().as_millis());
        rec.cancel();
        let levels: Vec<f32> = rx.try_iter().collect();
        println!("recorded {} ms, heard_sound {}, {} levels, max {:.2}", r.duration_ms, r.heard_sound, levels.len(), levels.iter().cloned().fold(0.0, f32::max));
        assert!(r.duration_ms >= 800, "{}", r.duration_ms);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_named_device_is_rejected() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let rec = AudioRecorder::with_test_audio(None, tx, None);
        assert!(rec.set_device(Some("No Such Microphone 12345".into())).is_err());
        assert!(rec.set_device(None).is_ok());
    }

    #[test]
    fn concurrent_starts_give_exactly_one_recording() {
        let dir = temp_dir("race");
        let src = dir.join("in.wav");
        write_tone(&src, 16_000, 1, 1.0, 0.3);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let rec = Arc::new(AudioRecorder::with_test_audio(None, tx, Some(src)));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let rec = rec.clone();
                let out = dir.join(format!("out{i}.wav"));
                std::thread::spawn(move || rec.start(out).is_ok())
            })
            .collect();
        let ok = handles.into_iter().map(|h| h.join().unwrap()).filter(|&ok| ok).count();
        assert_eq!(ok, 1, "one recording at a time, even when starts race (the device opens outside the lock)");
        std::thread::sleep(Duration::from_millis(100));
        assert!(rec.stop().is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }
}
