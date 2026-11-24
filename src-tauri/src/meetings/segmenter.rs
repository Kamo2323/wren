//! Voice-activity segmenter (meetings spec §5.2, plan Task 2). Cuts one 16 kHz mono stream into speech
//! segments using frame energy: a segment ends after `end_silence_ms` of silence or at `max_ms`, blips
//! with less than `min_speech_ms` of voiced audio are dropped, and each segment starts with up to
//! `pre_roll_ms` of the audio before the first voiced frame so word onsets aren't clipped.
//!
//! Pure: no I/O, no clocks. Timestamps are stream positions supplied by the caller.

use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmenterConfig {
    pub hop_ms: u32,
    pub rms_threshold: f32,
    pub end_silence_ms: u32,
    pub max_ms: u32,
    pub min_speech_ms: u32,
    pub pre_roll_ms: u32,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self { hop_ms: 32, rms_threshold: 0.01, end_silence_ms: 700, max_ms: 20_000, min_speech_ms: 400, pre_roll_ms: 200 }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub samples: Vec<f32>,
}

/// A stream position more than this far from where the last block ended counts as a jump (pause/gap).
const JUMP_TOLERANCE_MS: u64 = 50;

struct Open {
    start_ms: u64,
    samples: Vec<f32>,
    voiced_ms: u32,
    silence_ms: u32,
}

pub struct Segmenter {
    cfg: SegmenterConfig,
    sample_rate: u32,
    hop: usize,
    pending: Vec<f32>,
    /// Stream position of `pending[0]` (or of the next sample when `pending` is empty).
    next_ms: u64,
    pre_roll: VecDeque<f32>,
    pre_roll_cap: usize,
    open: Option<Open>,
}

impl Segmenter {
    pub fn new(cfg: SegmenterConfig, sample_rate: u32) -> Self {
        let hop = ((sample_rate as u64 * cfg.hop_ms as u64) / 1000).max(1) as usize;
        let pre_roll_cap = ((sample_rate as u64 * cfg.pre_roll_ms as u64) / 1000) as usize;
        Self {
            cfg,
            sample_rate,
            hop,
            pending: Vec::with_capacity(hop),
            next_ms: 0,
            pre_roll: VecDeque::with_capacity(pre_roll_cap),
            pre_roll_cap,
            open: None,
        }
    }

    fn samples_to_ms(&self, n: usize) -> u64 {
        n as u64 * 1000 / self.sample_rate as u64
    }

    /// True while a segment is open (someone is talking on this stream).
    pub fn is_speaking(&self) -> bool {
        self.open.is_some()
    }

    /// Stream position where the open segment started, while one is open.
    pub fn open_since(&self) -> Option<u64> {
        self.open.as_ref().map(|o| o.start_ms)
    }

    /// Feed audio that starts at `stream_pos_ms`; returns the segments that closed.
    pub fn push(&mut self, samples: &[f32], stream_pos_ms: u64) -> Vec<RawSegment> {
        let mut out = Vec::new();
        let expected = self.next_ms + self.samples_to_ms(self.pending.len());
        if self.pending.is_empty() && self.open.is_none() && self.pre_roll.is_empty() {
            self.next_ms = stream_pos_ms;
        } else if stream_pos_ms.abs_diff(expected) > JUMP_TOLERANCE_MS {
            // A gap (pause, device change): close what was open, forget partial frames and pre-roll.
            if let Some(seg) = self.flush() {
                out.push(seg);
            }
            self.pending.clear();
            self.pre_roll.clear();
            self.next_ms = stream_pos_ms;
        }
        let mut rest = samples;
        while !rest.is_empty() {
            let take = (self.hop - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == self.hop {
                let frame = std::mem::take(&mut self.pending);
                let start = self.next_ms;
                self.next_ms += self.samples_to_ms(frame.len());
                if let Some(seg) = self.frame(frame, start) {
                    out.push(seg);
                }
                self.pending = Vec::with_capacity(self.hop);
            }
        }
        out
    }

    fn frame(&mut self, frame: Vec<f32>, start_ms: u64) -> Option<RawSegment> {
        let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();
        let voiced = rms >= self.cfg.rms_threshold;
        let end_ms = start_ms + self.samples_to_ms(frame.len());
        let hop_ms = self.cfg.hop_ms;
        match self.open.as_mut() {
            None => {
                if voiced {
                    let pre: Vec<f32> = self.pre_roll.drain(..).collect();
                    let seg_start = start_ms.saturating_sub(self.samples_to_ms(pre.len()));
                    let mut samples = pre;
                    samples.extend_from_slice(&frame);
                    self.open = Some(Open { start_ms: seg_start, samples, voiced_ms: hop_ms, silence_ms: 0 });
                } else {
                    for s in frame {
                        if self.pre_roll.len() == self.pre_roll_cap {
                            self.pre_roll.pop_front();
                        }
                        if self.pre_roll_cap > 0 {
                            self.pre_roll.push_back(s);
                        }
                    }
                }
                None
            }
            Some(open) => {
                open.samples.extend_from_slice(&frame);
                if voiced {
                    open.voiced_ms += hop_ms;
                    open.silence_ms = 0;
                } else {
                    open.silence_ms += hop_ms;
                }
                if open.silence_ms >= self.cfg.end_silence_ms {
                    let open = self.open.take().expect("open");
                    return self.close(open, end_ms);
                }
                if end_ms - open.start_ms >= self.cfg.max_ms as u64 {
                    let done = self.open.take().expect("open");
                    // Carry straight on: the next segment starts where this one ended.
                    self.open = Some(Open { start_ms: end_ms, samples: Vec::new(), voiced_ms: 0, silence_ms: 0 });
                    return self.close(done, end_ms);
                }
                None
            }
        }
    }

    fn close(&mut self, open: Open, end_ms: u64) -> Option<RawSegment> {
        self.pre_roll.clear();
        if open.voiced_ms < self.cfg.min_speech_ms {
            return None;
        }
        Some(RawSegment { start_ms: open.start_ms, end_ms, samples: open.samples })
    }

    /// Close and return the open segment (end of meeting, pause). Partial frames are included.
    pub fn flush(&mut self) -> Option<RawSegment> {
        let mut open = self.open.take()?;
        let tail = std::mem::take(&mut self.pending);
        let end_ms = self.next_ms + self.samples_to_ms(tail.len());
        self.next_ms = end_ms;
        open.samples.extend_from_slice(&tail);
        self.close(open, end_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 16_000;

    fn tone(ms: u64) -> Vec<f32> {
        let n = (SR as u64 * ms / 1000) as usize;
        (0..n).map(|i| 0.2 * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / SR as f32).sin()).collect()
    }
    fn silence(ms: u64) -> Vec<f32> {
        vec![0.0; (SR as u64 * ms / 1000) as usize]
    }
    /// Feed `blocks` in 100 ms pieces, returning every segment emitted (and the final flush).
    fn run(blocks: &[Vec<f32>]) -> (Vec<RawSegment>, Segmenter) {
        let mut s = Segmenter::new(SegmenterConfig::default(), SR);
        let all: Vec<f32> = blocks.concat();
        let mut out = Vec::new();
        let mut pos = 0u64;
        for chunk in all.chunks(1_600) {
            out.extend(s.push(chunk, pos));
            pos += chunk.len() as u64 * 1000 / SR as u64;
        }
        (out, s)
    }

    #[test]
    fn silence_yields_nothing() {
        let (segs, mut s) = run(&[silence(5_000)]);
        assert!(segs.is_empty());
        assert!(s.flush().is_none());
        assert!(!s.is_speaking());
    }

    #[test]
    fn tone_then_700ms_silence_yields_one_segment_with_pre_roll() {
        let (segs, _) = run(&[silence(1_000), tone(1_500), silence(1_000)]);
        assert_eq!(segs.len(), 1, "{segs:?}");
        let g = &segs[0];
        assert!((g.start_ms as i64 - 800).abs() <= 32, "start {}", g.start_ms);
        assert!(g.end_ms >= 2_500 && g.end_ms <= 2_500 + 750, "end {}", g.end_ms);
        let expect = ((g.end_ms - g.start_ms) * SR as u64 / 1000) as i64;
        assert!((g.samples.len() as i64 - expect).abs() <= 1_600, "samples match the span");
    }

    #[test]
    fn blip_under_400ms_dropped() {
        let (segs, mut s) = run(&[silence(500), tone(200), silence(1_500)]);
        assert!(segs.is_empty(), "{segs:?}");
        assert!(s.flush().is_none());
    }

    #[test]
    fn continuous_speech_splits_at_20s() {
        let (segs, mut s) = run(&[tone(45_000)]);
        assert_eq!(segs.len(), 2, "two full 20 s segments emitted");
        for g in &segs {
            assert!(g.end_ms - g.start_ms <= 20_000 + 32, "{}", g.end_ms - g.start_ms);
        }
        assert_eq!(segs[1].start_ms, segs[0].end_ms, "no gap between forced splits");
        let rest = s.flush().expect("the open remainder");
        assert!(rest.end_ms - rest.start_ms >= 4_000);
    }

    #[test]
    fn flush_returns_open_segment() {
        let (segs, mut s) = run(&[silence(300), tone(1_000)]);
        assert!(segs.is_empty());
        assert!(s.is_speaking());
        let g = s.flush().unwrap();
        assert!(g.end_ms >= 1_250);
        assert!(!s.is_speaking());
        assert!(s.flush().is_none());
    }

    #[test]
    fn is_speaking_tracks_state() {
        let mut s = Segmenter::new(SegmenterConfig::default(), SR);
        s.push(&tone(300), 0);
        assert!(s.is_speaking());
        s.push(&silence(1_000), 300);
        assert!(!s.is_speaking());
    }

    #[test]
    fn stream_position_jumps_are_respected() {
        // After a pause the caller resumes with a later stream position; timestamps follow it.
        let mut s = Segmenter::new(SegmenterConfig::default(), SR);
        let mut out = s.push(&tone(1_000), 0);
        out.extend(s.push(&silence(1_000), 1_000));
        out.extend(s.push(&tone(1_000), 60_000));
        out.extend(s.push(&silence(1_000), 61_000));
        assert_eq!(out.len(), 2);
        assert!(out[1].start_ms >= 59_700 && out[1].start_ms <= 60_032, "{}", out[1].start_ms);
    }
}
