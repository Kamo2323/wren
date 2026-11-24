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
