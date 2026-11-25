//! A long-running 16 kHz mono 16-bit WAV (meeting `me.wav` / `them.wav`, spec §5.1). The header is
//! rewritten every `HEADER_FLUSH_SAMPLES` so a crash leaves a playable file; `Drop` finalizes.
//! At 16 kHz mono 16-bit an hour is ~115 MB, far inside the WAV 4 GB limit.

use anyhow::{Context, Result};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

const SAMPLE_RATE: u32 = 16_000;
/// Rewrite the header every 10 s of audio.
const HEADER_FLUSH_SAMPLES: usize = 10 * SAMPLE_RATE as usize;

/// Where the PCM data starts and how many 16-bit mono samples the file really holds (from its size,
/// not its header — a live or crashed file's header lags behind).
pub fn pcm_layout(path: &Path) -> std::io::Result<(u64, u64)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not a Wren WAV");
    let mut head = [0u8; 12];
    f.read_exact(&mut head)?;
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err(bad());
    }
    let mut pos = 12u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos))?;
        let mut ch = [0u8; 8];
        f.read_exact(&mut ch)?;
        let size = u32::from_le_bytes([ch[4], ch[5], ch[6], ch[7]]) as u64;
        if &ch[0..4] == b"data" {
            let offset = pos + 8;
            return Ok((offset, (len - offset) / 2));
        }
        pos += 8 + size + (size & 1);
    }
    Err(bad())
}

/// Rewrite the RIFF and data sizes from the file's real length (after a crash). True if changed.
pub fn repair_header(path: &Path) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let (offset, samples) = pcm_layout(path)?;
    let data = (samples * 2) as u32;
    let riff = (offset + samples * 2 - 8) as u32;
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let mut cur = [0u8; 4];
    f.seek(SeekFrom::Start(offset - 4))?;
    f.read_exact(&mut cur)?;
    if u32::from_le_bytes(cur) == data {
        return Ok(false);
    }
    f.seek(SeekFrom::Start(4))?;
    f.write_all(&riff.to_le_bytes())?;
    f.seek(SeekFrom::Start(offset - 4))?;
    f.write_all(&data.to_le_bytes())?;
    f.flush()?;
    Ok(true)
}

/// Repair both recordings of a meeting folder (startup, for meetings a crash interrupted).
pub fn repair_meeting_audio(dir: &Path) {
    for name in ["me.wav", "them.wav"] {
        let p = dir.join(name);
        if p.is_file() {
            match repair_header(&p) {
                Ok(true) => tracing::info!(file = name, "repaired an interrupted meeting recording"),
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, file = name, "could not repair a meeting recording"),
            }
        }
    }
}

pub struct ContinuousWav {
    writer: Option<hound::WavWriter<BufWriter<File>>>,
    since_flush: usize,
}

impl ContinuousWav {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).context("cannot create the meeting folder")?;
        }
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec).context("cannot create the meeting recording")?;
        Ok(Self { writer: Some(writer), since_flush: 0 })
    }

    pub fn write(&mut self, samples: &[f32]) {
        let Some(w) = self.writer.as_mut() else { return };
        for &s in samples {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            if w.write_sample(v).is_err() {
                tracing::warn!("meeting recording write failed; recording stops here");
                self.writer = None;
                return;
            }
        }
        self.since_flush += samples.len();
        if self.since_flush >= HEADER_FLUSH_SAMPLES {
            self.since_flush = 0;
            if let Err(e) = w.flush() {
                tracing::warn!(error = %e, "meeting recording flush failed");
            }
        }
    }

    pub fn finalize(mut self) -> Result<()> {
        match self.writer.take() {
            Some(w) => w.finalize().context("cannot finish the meeting recording"),
            None => Ok(()),
        }
    }
}

impl Drop for ContinuousWav {
    fn drop(&mut self) {
        if let Some(w) = self.writer.take() {
            let _ = w.finalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wren-wav-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn secs(path: &std::path::Path) -> f64 {
        let r = hound::WavReader::open(path).unwrap();
        r.duration() as f64 / r.spec().sample_rate as f64
    }

    #[test]
    fn continuous_wav_finalize_roundtrip() {
        let p = temp("a.wav");
        let mut w = ContinuousWav::create(&p).unwrap();
        w.write(&vec![0.25; 16_000]);
        w.finalize().unwrap();
        let r = hound::WavReader::open(&p).unwrap();
        assert_eq!(r.spec().sample_rate, 16_000);
        assert_eq!(r.spec().channels, 1);
        assert!((secs(&p) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn continuous_wav_valid_after_drop_without_finalize() {
        let p = temp("b.wav");
        {
            let mut w = ContinuousWav::create(&p).unwrap();
            w.write(&vec![0.1; 8_000]);
        }
        assert!((secs(&p) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn continuous_wav_header_flushed_periodically() {
        let p = temp("c.wav");
        let mut w = ContinuousWav::create(&p).unwrap();
        for _ in 0..11 {
            w.write(&vec![0.1; 16_000]);
        }
        // Not finalized: a crash now must still leave a playable file with at least 10 s in it.
        let got = secs(&p);
        assert!(got >= 10.0, "{got}");
        std::mem::forget(w); // simulate a crash: no Drop
    }

    #[test]
    fn clipping_is_clamped() {
        let p = temp("d.wav");
        let mut w = ContinuousWav::create(&p).unwrap();
        w.write(&[2.0, -2.0]);
        w.finalize().unwrap();
        let s: Vec<i16> = hound::WavReader::open(&p).unwrap().samples::<i16>().map(Result::unwrap).collect();
        assert_eq!(s, vec![i16::MAX, -i16::MAX]);
    }
}

#[cfg(test)]
mod repair_tests {
    use super::*;
    use std::io::Write;

    /// What a hard crash leaves: a header that still says 0 samples, followed by the audio.
    fn crashed_file(p: &Path, samples: usize) {
        ContinuousWav::create(p).unwrap().finalize().unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        let pcm: Vec<u8> = (0..samples).flat_map(|i| ((i % 200) as i16).to_le_bytes()).collect();
        f.write_all(&pcm).unwrap();
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wren-wav-rep-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn crash_before_the_first_header_flush_is_repaired() {
        let p = temp("me.wav");
        crashed_file(&p, 3 * 16_000);
        assert_eq!(hound::WavReader::open(&p).unwrap().duration(), 0, "broken before repair");
        assert!(repair_header(&p).unwrap(), "header patched");
        let r = hound::WavReader::open(&p).unwrap();
        assert_eq!(r.duration(), 3 * 16_000);
        assert!(!repair_header(&p).unwrap(), "already correct: nothing to do");
    }

    #[test]
    fn samples_available_counts_unflushed_audio() {
        let p = temp("them.wav");
        crashed_file(&p, 2 * 16_000);
        let (offset, samples) = pcm_layout(&p).unwrap();
        assert_eq!(offset, 44);
        assert_eq!(samples, 2 * 16_000, "a live file reads its real length, not the stale header");
    }
}
