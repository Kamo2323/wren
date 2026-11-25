//! macOS "Them" capture (meetings spec §5.1, plan Task 6): a Core Audio process tap (`CATapDescription`
//! plus a private aggregate device, macOS 14.2+) on the Teams process and its helpers, so only Teams'
//! sound is recorded, whichever output device Teams plays to. Fallbacks mirror `win_loopback.rs`:
//! - Teams not running → `MicOnly` (silence is still emitted so both streams stay aligned), and Wren
//!   attaches when Teams appears.
//! - Teams running but its tap can't be created → `AllSound`: a global tap of every process except
//!   Wren's own, i.e. everything the computer plays except Wren's sounds.
//!
//! Needs `NSAudioCaptureUsageDescription` in Info.plist and the "System Audio Recording" permission
//! (System Settings › Privacy & Security › Screen & System Audio Recording). Without it the tap is
//! created but delivers silence.
//!
//! Audio arrives at the tap's rate (usually 48 kHz) as f32 on Core Audio's real-time thread, which
//! mixes it to mono into a preallocated lock-free ring (no allocation, locks or unwinding there); the
//! worker drains it and resamples to 16 kHz with [`Resampler16k`].
//!
//! Self-healing (as `win_loopback.rs`): the worker reopens the capture when Teams appears, quits or
//! restarts, when no tap is open while Teams has audio (both taps failed), when the tap stops calling
//! back for [`STALL`] (or its aggregate device reads as not alive, or unreadable), and when the tap's
//! rate changes in a way the in-place handling below didn't catch. Rate changes (AirPods 48 kHz ↔
//! hands-free 16/24 kHz, sleep/wake) are caught per IO cycle: a timeline jump or a cycle whose
//! timestamps imply another rate marks a boundary in the ring, and the worker settles the new rate
//! there before resampling anything after it. A rate that can't be told even after [`BOUNDARY_HOLD`]
//! reopens the tap (a bounded number of times), flushing held audio before replacement. On the
//! all-sound fallback it retries Teams-only every [`UPGRADE_RETRY`], opening that tap on another thread
//! while the fallback keeps being drained, and switches only once the new tap delivers. Mode changes
//! are reported on `mode_tx`. Wall-clock alignment: when too little arrives, zeros are emitted so
//! `stream_pos_ms` tracks real time (never while audio is held at a boundary). Audio is never logged.

use super::{AudioBlock, AudioSource};
use crate::audio::Resampler16k;
use crate::meetings::CaptureMode;
use crossbeam_channel::Sender;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProc, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyProcessObjectList,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{
    AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags, kAudioFormatFlagIsFloat,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RATE: u64 = 16_000;
/// How often the loop wakes when nothing arrives.
const TICK: Duration = Duration::from_millis(100);
/// Pad with silence only when this far behind real time (the tap delivers every ~10 ms).
const PAD_AFTER_MS: u64 = 300;
/// How often to look for Teams appearing, restarting or quitting.
const RECHECK: Duration = Duration::from_secs(5);
/// A tap with no IO callback for this long, while its device is gone or the processes it taps are
/// playing, is dead (device change, sleep/wake) and is reopened. A tap on processes that aren't
/// playing gets no callbacks at all, so silence alone is not a stall.
const STALL: Duration = Duration::from_secs(3);
/// How often to check the tap's health (sample rate, stall).
const HEALTH_CHECK: Duration = Duration::from_secs(1);
/// While on the all-sound fallback, how often to try Teams-only again (as on Windows).
const UPGRADE_RETRY: Duration = Duration::from_secs(30);
/// How long `stop` waits for the worker before leaving it to finish on its own (as `mic.rs`).
const STOP_JOIN: Duration = Duration::from_secs(2);
/// `open_tap`'s error when Stop came in while it was opening.
const STOPPED: &str = "capture stopped while the tap was opening";
/// Retries that keep failing log a warning at most this often.
const QUIET_RETRY_LOGS: Duration = Duration::from_secs(60);
/// Seconds of audio buffered between the Core Audio IO thread and the worker (it drains every tick,
/// and holds back at most [`BOUNDARY_HOLD`] at a format boundary).
const RING_SECS: usize = 2;
/// The ring is sized for at least this rate, whatever rate the tap opens at: a tap opened during a
/// hands-free call (16/24 kHz) can switch to 48 kHz (or a 96 kHz interface) before the worker reaches
/// the boundary, and must still hold [`RING_SECS`] then. The ring holds mono frames (the IO proc mixes
/// every channel down before writing), so the channel count doesn't multiply it.
const RING_MIN_RATE: usize = 96_000;

/// Ring capacity (samples) for a tap that opened at `rate`.
fn ring_samples(rate: u32) -> usize {
    (rate as usize).max(RING_MIN_RATE) * RING_SECS
}
/// Reopens of the tap because its rate couldn't be settled after a format change: at most this many
/// per [`RATE_REOPEN_WINDOW`]; past that the rate property decides (with a warning).
const RATE_REOPENS: u32 = 3;
const RATE_REOPEN_WINDOW: Duration = Duration::from_secs(60);

/// System Settings › Privacy & Security › Screen & System Audio Recording.
pub const SYSTEM_AUDIO_SETTINGS_URI: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";

/// Which target to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Teams and its helper processes, falling back as described above.
    Teams,
    /// Everything except Wren (used by tests and as the fallback).
    AllSound,
}

// ---- pure helpers ----

/// A Core Audio process object: the processes that have talked to the audio system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioProc {
    pub object: AudioObjectID,
    pub pid: i32,
    /// Parent, grandparent, … (at most [`ANCESTOR_HOPS`]).
    pub ancestors: Vec<i32>,
    pub bundle_id: String,
}

/// How far up the process tree a Teams helper may sit below the Teams app (Teams → WebView host →
/// renderer / GPU / audio helpers is already two hops).
pub const ANCESTOR_HOPS: usize = 4;

/// New Teams (`com.microsoft.teams2`), its helpers (`com.microsoft.teams2.*`) and classic Teams.
pub fn is_teams_bundle(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ["com.microsoft.teams2", "com.microsoft.teams"]
        .iter()
        .any(|b| id == *b || id.strip_prefix(b).is_some_and(|rest| rest.starts_with('.')))
}

/// The process objects to tap for Teams: anything with a Teams bundle id, plus processes descended
/// (within [`ANCESTOR_HOPS`]) from a running Teams app (Chromium-style helpers, which may carry their
/// own bundle id). Sorted, unique.
pub fn teams_objects(procs: &[AudioProc], teams_pids: &[i32]) -> Vec<AudioObjectID> {
    let mut out: Vec<AudioObjectID> = procs
        .iter()
        .filter(|p| {
            is_teams_bundle(&p.bundle_id) || teams_pids.contains(&p.pid) || p.ancestors.iter().any(|a| teams_pids.contains(a))
        })
        .map(|p| p.object)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// One Core Audio buffer: interleaved samples and their channel count.
trait Plane {
    fn samples(&self) -> &[f32];
    fn channels(&self) -> usize;
}

impl Plane for (&[f32], usize) {
    fn samples(&self) -> &[f32] {
        self.0
    }
    fn channels(&self) -> usize {
        self.1.max(1)
    }
}

impl Plane for AudioBuffer {
    fn samples(&self) -> &[f32] {
        let p = self.mData as *const f32;
        if p.is_null() || !p.is_aligned() {
            return &[];
        }
        // Core Audio owns the buffer for the duration of the IO callback this is called from.
        unsafe { std::slice::from_raw_parts(p, self.mDataByteSize as usize / std::mem::size_of::<f32>()) }
    }
    fn channels(&self) -> usize {
        (self.mNumberChannels as usize).max(1)
    }
}

/// Single-producer single-consumer ring of f32 samples: the Core Audio IO thread writes, the worker
/// reads. Preallocated, power-of-two sized; neither side allocates, locks or can panic. When the
/// worker falls behind, new samples are dropped (and counted) rather than blocking the IO thread.
struct Ring {
    buf: Box<[AtomicU32]>,
    mask: usize,
    /// Samples ever written / read; `head - tail` is the fill.
    head: AtomicUsize,
    tail: AtomicUsize,
    dropped: AtomicUsize,
    /// A format boundary: `position + 1` of the first sample that may be in a new format (0 = none).
    /// Set by the producer (only when clear), consumed by the consumer when it reaches it.
    mark: AtomicUsize,
}

impl Ring {
    fn new(min_capacity: usize) -> Self {
        let cap = min_capacity.max(2).next_power_of_two();
        Self {
            buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            mark: AtomicUsize::new(0),
        }
    }

    /// Producer: the next sample written may be in a different format than the ones before it. Only
    /// the first unconsumed boundary is kept (two format changes within one worker tick don't happen).
    fn mark_boundary(&self) {
        let pos = self.head.load(Ordering::Relaxed);
        let _ = self.mark.compare_exchange(0, pos.wrapping_add(1), Ordering::Release, Ordering::Relaxed);
    }

    /// Producer: append `n` samples given by `sample(i)`; what doesn't fit is dropped.
    fn push_with(&self, n: usize, mut sample: impl FnMut(usize) -> f32) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let free = self.buf.len() - head.wrapping_sub(tail).min(self.buf.len());
        let take = n.min(free);
        for i in 0..take {
            if let Some(slot) = self.buf.get(head.wrapping_add(i) & self.mask) {
                slot.store(sample(i).to_bits(), Ordering::Relaxed);
            }
        }
        self.head.store(head.wrapping_add(take), Ordering::Release);
        if take < n {
            self.dropped.fetch_add(n - take, Ordering::Relaxed);
        }
    }

    /// Consumer: throw away everything queued so far. A boundary at or after the position discarded up
    /// to (nothing after it yet, or marked since) is kept: it still applies to what comes next.
    fn discard(&self) {
        let head = self.head.load(Ordering::Acquire);
        let mark = self.mark.load(Ordering::Acquire);
        self.tail.store(head, Ordering::Release);
        // How far before `head` the boundary sits; a mark set after `head` was read wraps to huge.
        let before = head.wrapping_sub(mark.wrapping_sub(1));
        if mark != 0 && before != 0 && before <= self.buf.len() {
            let _ = self.mark.compare_exchange(mark, 0, Ordering::AcqRel, Ordering::Relaxed);
        }
    }

    /// Consumer: move what is available onto the end of `out`, stopping at a format boundary. Returns
    /// true when it stopped at (and consumed) one: what follows may be in a different format.
    fn drain_into(&self, out: &mut Vec<f32>) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        // `head` before `mark`: a mark set before the samples we see is then visible too.
        let head = self.head.load(Ordering::Acquire);
        let mark = self.mark.load(Ordering::Acquire);
        let avail = head.wrapping_sub(tail).min(self.buf.len());
        let to_mark = (mark != 0).then(|| mark.wrapping_sub(1).wrapping_sub(tail)).filter(|d| *d <= avail);
        let n = to_mark.unwrap_or(avail);
        out.reserve(n);
        for i in 0..n {
            if let Some(slot) = self.buf.get(tail.wrapping_add(i) & self.mask) {
                out.push(f32::from_bits(slot.load(Ordering::Relaxed)));
            }
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
        if to_mark.is_some() {
            // The producer only sets a clear mark, so nothing races this store.
            self.mark.store(0, Ordering::Release);
        }
        to_mark.is_some()
    }
}

/// Frames in one IO cycle: the shortest buffer with data sets it.
fn frame_count<P: Plane>(planes: &[P]) -> usize {
    planes.iter().filter(|p| !p.samples().is_empty()).map(|p| p.samples().len() / p.channels()).min().unwrap_or(0)
}

/// Mix Core Audio buffers (each interleaved with its own channel count) down to mono into `ring`,
/// without allocating. Buffers without data are skipped; the shortest one sets the frame count.
fn mix_to_mono<P: Plane>(planes: &[P], ring: &Ring) {
    let live = || planes.iter().filter(|p| !p.samples().is_empty());
    let total_ch: usize = live().map(|p| p.channels()).sum();
    let frames = frame_count(planes);
    if total_ch == 0 || frames == 0 {
        return;
    }
    let scale = 1.0 / total_ch as f32;
    ring.push_with(frames, |f| {
        let mut sum = 0.0;
        for p in live() {
            let ch = p.channels();
            if let Some(frame) = p.samples().get(f * ch..f * ch + ch) {
                sum += frame.iter().sum::<f32>();
            }
        }
        sum * scale
    });
}

// ---- Core Audio property access ----

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn status(what: &str, st: i32) -> anyhow::Result<()> {
    if st == 0 {
        Ok(())
    } else {
        let b = st.to_be_bytes();
        let fourcc = if b.iter().all(|c| c.is_ascii_graphic()) { String::from_utf8_lossy(&b).into_owned() } else { String::new() };
        Err(anyhow::anyhow!("{what} failed: OSStatus {st} {fourcc}"))
    }
}

/// Read a fixed-size property. `qualifier` is passed as the property's qualifier data.
fn get<T: Copy>(obj: AudioObjectID, selector: u32, qualifier: Option<&[u8]>) -> Option<T> {
    let mut addr = address(selector);
    let mut size = std::mem::size_of::<T>() as u32;
    let mut out = std::mem::MaybeUninit::<T>::zeroed();
    let (qs, qp) = qualifier.map_or((0, std::ptr::null()), |q| (q.len() as u32, q.as_ptr().cast::<c_void>()));
    let st = unsafe {
        AudioObjectGetPropertyData(
            obj,
            NonNull::from(&mut addr),
            qs,
            qp,
            NonNull::from(&mut size),
            NonNull::new_unchecked(out.as_mut_ptr().cast()),
        )
    };
    (st == 0 && size as usize == std::mem::size_of::<T>()).then(|| unsafe { out.assume_init() })
}

fn get_u32_list(obj: AudioObjectID, selector: u32) -> Vec<u32> {
    let mut addr = address(selector);
    let mut size = 0u32;
    let st = unsafe { AudioObjectGetPropertyDataSize(obj, NonNull::from(&mut addr), 0, std::ptr::null(), NonNull::from(&mut size)) };
    if st != 0 || size == 0 {
        return Vec::new();
    }
    let mut out = vec![0u32; size as usize / 4];
    let st = unsafe {
        AudioObjectGetPropertyData(
            obj,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(out.as_mut_ptr().cast()),
        )
    };
    if st != 0 {
        return Vec::new();
    }
    out.truncate(size as usize / 4);
    out
}

fn get_string(obj: AudioObjectID, selector: u32) -> Option<String> {
    let ptr: *mut NSString = get::<*mut NSString>(obj, selector, None)?;
    // The property hands back a +1 CFString (toll-free bridged with NSString).
    let s = unsafe { Retained::from_raw(ptr) }?;
    Some(s.to_string())
}

/// Every process the audio system knows about.
pub fn audio_processes() -> Vec<AudioProc> {
    get_u32_list(kAudioObjectSystemObject as u32, kAudioHardwarePropertyProcessObjectList)
        .into_iter()
        .map(|object| {
            let pid = get::<i32>(object, kAudioProcessPropertyPID, None).unwrap_or(-1);
            let ancestors = if pid > 0 { crate::meetings::mac_procs::ancestors(pid, ANCESTOR_HOPS) } else { Vec::new() };
            AudioProc { object, pid, ancestors, bundle_id: get_string(object, kAudioProcessPropertyBundleID).unwrap_or_default() }
        })
        .collect()
}

/// Pids of running Teams apps (main app bundles). libproc, not `NSWorkspace` (AppKit is main-thread
/// only; this runs on the capture thread).
pub fn running_teams_pids() -> Vec<i32> {
    crate::meetings::mac_procs::running_app_pids(is_teams_bundle)
}

/// Wren's own process object, if Wren has registered with the audio system.
fn own_process_object() -> Option<AudioObjectID> {
    let pid = std::process::id() as i32;
    get::<AudioObjectID>(kAudioObjectSystemObject as u32, kAudioHardwarePropertyTranslatePIDToProcessObject, Some(&pid.to_ne_bytes()))
        .filter(|o| *o != kAudioObjectUnknown)
}

// ---- the tap ----

/// One IO cycle's input timestamp, as far as boundary detection needs it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cycle {
    sample: f64,
    host: u64,
    frames: usize,
}

/// How far the rate an IO cycle's timestamps imply may stray from the expected rate before it counts
/// as a format change (the closest common rates, 44.1 and 48 kHz, are 8 % apart).
const RATE_TOLERANCE: f64 = 0.04;

/// What an IO cycle's timestamp says against the cycle before it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Step {
    /// The device timeline jumped: IO restarted (device or format change, sleep/wake, an overload).
    Jump,
    /// Continuous, at this rate (Hz) as the timestamps imply.
    Rate(f64),
    /// Nothing to go on (host time didn't advance).
    Unknown,
}

fn step(prev: Cycle, now: Cycle, host_hz: f64) -> Step {
    if (now.sample - (prev.sample + prev.frames as f64)).abs() > 1.0 {
        return Step::Jump;
    }
    let ds = now.sample - prev.sample;
    if ds <= 0.0 || now.host <= prev.host || host_hz <= 0.0 {
        return Step::Unknown;
    }
    Step::Rate(ds / ((now.host - prev.host) as f64 / host_hz))
}

/// Whether `rate` is clearly not `expected`.
fn off_rate(rate: f64, expected: f64) -> bool {
    expected > 0.0 && (rate / expected - 1.0).abs() > RATE_TOLERANCE
}

/// What to do at a format boundary, before resampling anything after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtBoundary {
    /// Still `current`: carry on.
    Same,
    /// Now at this rate: flush the old resampler and start one at the new rate.
    Retune(u32),
    /// Core Audio's rate property and the IO timestamps disagree (the property hasn't caught up) or
    /// no cycle after the boundary has been measured yet: hold the audio and look again next tick.
    Wait,
    /// Held for [`BOUNDARY_HOLD`] and the rate still can't be told (the property is unreadable, or it
    /// and the timestamps still disagree): reopen the tap rather than resample at a guessed rate.
    Unknown,
}

/// Decide at a boundary from the rate property (`prop`), the rate the IO timestamps imply since the
/// boundary (`implied`) and whether the boundary has waited [`BOUNDARY_HOLD`] already.
fn at_boundary(current: u32, prop: Option<u32>, implied: Option<f64>, timed_out: bool) -> AtBoundary {
    let by_prop = |p: u32| if p != current { AtBoundary::Retune(p) } else { AtBoundary::Same };
    match (prop, implied) {
        (Some(p), Some(i)) if !off_rate(i, p as f64) => by_prop(p),
        (None, Some(i)) if !off_rate(i, current as f64) => AtBoundary::Same,
        _ if !timed_out => AtBoundary::Wait,
        // Nothing measured since the boundary (the source went quiet right after it): the property
        // is all there is, and nothing it could contradict.
        (Some(p), None) => by_prop(p),
        _ => AtBoundary::Unknown,
    }
}

/// What to resample at when the rate stayed [`AtBoundary::Unknown`] and we must drain the ring:
/// the property if readable, else the current rate.
fn fallback_rate(current: u32, prop: Option<u32>) -> AtBoundary {
    match prop {
        Some(p) if p != current => AtBoundary::Retune(p),
        _ => AtBoundary::Same,
    }
}

/// Reopens for an unsettled rate: at most [`RATE_REOPENS`] per [`RATE_REOPEN_WINDOW`].
#[derive(Debug, Clone, Copy, Default)]
struct ReopenBudget {
    count: u32,
    since: Option<Instant>,
}

impl ReopenBudget {
    fn fresh(&self, now: Instant) -> bool {
        self.since.is_none_or(|s| now.saturating_duration_since(s) >= RATE_REOPEN_WINDOW)
    }

    fn allowed(&self, now: Instant) -> bool {
        self.fresh(now) || self.count < RATE_REOPENS
    }

    /// Count one reopen; returns which one this is in the current window (1-based).
    fn spend(&mut self, now: Instant) -> u32 {
        if self.fresh(now) {
            *self = Self { count: 0, since: Some(now) };
        }
        self.count += 1;
        self.count
    }
}

/// Longest a boundary holds audio back waiting for the rate property to agree with the IO timestamps.
const BOUNDARY_HOLD: Duration = Duration::from_secs(1);

/// Shared between the worker and the Core Audio IO thread.
struct Ctx {
    ring: Ring,
    /// IO callbacks so far (liveness: a tap whose device stopped calling back is reopened).
    callbacks: AtomicU64,
    /// Host clock ticks per second.
    host_hz: f64,
    /// The rate the IO timestamps are expected to imply (Hz); a cycle clearly off it is a boundary.
    expected_rate: AtomicU32,
    /// The rate the latest continuous cycle implied (f64 bits; 0 = unknown since the last jump).
    implied_rate: AtomicU64,
    /// IO thread only: the previous cycle (`prev_frames == 0`: none yet).
    prev_sample: AtomicU64,
    prev_host: AtomicU64,
    prev_frames: AtomicUsize,
    /// Format boundaries seen (diagnostics).
    boundaries: AtomicU64,
}

impl Ctx {
    fn new(rate: u32) -> Self {
        Self {
            ring: Ring::new(ring_samples(rate)),
            callbacks: AtomicU64::new(0),
            host_hz: unsafe { objc2_core_audio::AudioGetHostClockFrequency() },
            expected_rate: AtomicU32::new(rate),
            implied_rate: AtomicU64::new(0),
            prev_sample: AtomicU64::new(0),
            prev_host: AtomicU64::new(0),
            prev_frames: AtomicUsize::new(0),
            boundaries: AtomicU64::new(0),
        }
    }

    /// IO thread: note this cycle's timestamp, marking the ring when it starts a new stretch.
    fn observe(&self, ts: &AudioTimeStamp, frames: usize) {
        if frames == 0 || !ts.mFlags.contains(AudioTimeStampFlags::SampleHostTimeValid) {
            return;
        }
        let now = Cycle { sample: ts.mSampleTime, host: ts.mHostTime, frames };
        let prev_frames = self.prev_frames.load(Ordering::Relaxed);
        if prev_frames > 0 {
            let prev = Cycle {
                sample: f64::from_bits(self.prev_sample.load(Ordering::Relaxed)),
                host: self.prev_host.load(Ordering::Relaxed),
                frames: prev_frames,
            };
            let boundary = match step(prev, now, self.host_hz) {
                Step::Jump => {
                    self.implied_rate.store(0, Ordering::Relaxed);
                    true
                }
                Step::Rate(r) => {
                    self.implied_rate.store(r.to_bits(), Ordering::Relaxed);
                    off_rate(r, self.expected_rate.load(Ordering::Relaxed) as f64)
                }
                Step::Unknown => false,
            };
            if boundary {
                self.ring.mark_boundary();
                self.boundaries.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.prev_sample.store(now.sample.to_bits(), Ordering::Relaxed);
        self.prev_host.store(now.host, Ordering::Relaxed);
        self.prev_frames.store(frames, Ordering::Relaxed);
    }
}

type IoProc = unsafe extern "C" fn(
    AudioObjectID,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    *mut c_void,
) -> i32;

/// The binding's IO proc type (`extern "C-unwind"`).
type IoProcUnwind = unsafe extern "C-unwind" fn(
    AudioObjectID,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    NonNull<AudioBufferList>,
    NonNull<AudioTimeStamp>,
    *mut c_void,
) -> i32;

/// Runs on Core Audio's real-time IO thread: no allocation, locks, logging or panicking paths. It is
/// `extern "C"` so that even an unexpected panic aborts instead of unwinding into Core Audio.
unsafe extern "C" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let Some(ctx) = (unsafe { (client as *const Ctx).as_ref() }) else { return 0 };
    let list = input.as_ptr();
    let n = unsafe { (*list).mNumberBuffers } as usize;
    // `mBuffers` is a C flexible array: take the pointer from the list, not from the 1-element field.
    let first = unsafe { std::ptr::addr_of!((*list).mBuffers) }.cast::<AudioBuffer>();
    let bufs: &[AudioBuffer] = unsafe { std::slice::from_raw_parts(first, n) };
    // Before the samples go in: a boundary marks where they start.
    ctx.observe(unsafe { input_time.as_ref() }, frame_count(bufs));
    mix_to_mono(bufs, &ctx.ring);
    ctx.callbacks.fetch_add(1, Ordering::Relaxed);
    0
}

/// The sample rate the tap reports (`kAudioTapPropertyFormat`).
fn tap_rate(tap: AudioObjectID) -> Option<u32> {
    get::<AudioStreamBasicDescription>(tap, kAudioTapPropertyFormat, None)
        .map(|f| f.mSampleRate.round() as u32)
        .filter(|r| *r >= 8_000)
}

/// The aggregate device's nominal sample rate: the rate the IO proc receives.
fn device_rate(device: AudioObjectID) -> Option<u32> {
    get::<f64>(device, kAudioDevicePropertyNominalSampleRate, None).map(|r| r.round() as u32).filter(|r| *r >= 8_000)
}

/// Whether a process object is currently playing audio.
fn process_playing(object: AudioObjectID) -> bool {
    get::<u32>(object, kAudioProcessPropertyIsRunningOutput, None).is_some_and(|v| v != 0)
}

/// A tap without callbacks for `silent_for` is dead when its device is gone, or when the processes
/// it taps are playing (they'd be calling back). Silent processes deliver nothing: not a stall.
fn is_stalled(silent_for: Duration, device_alive: bool, sources_playing: bool) -> bool {
    silent_for > STALL && (!device_alive || sources_playing)
}

/// `kAudioDevicePropertyDeviceIsAlive`: only a readable non-zero value is alive.
fn is_alive(prop: Option<u32>) -> bool {
    prop.is_some_and(|v| v != 0)
}

/// Silence (16 kHz samples) to emit before a block of `block` samples so the block ends at real time
/// (`real` samples since the start) when the stream has fallen more than [`PAD_AFTER_MS`] behind;
/// `block == 0` pads up to now. Never while `holding` audio back at a format boundary: that audio is
/// still coming, and silence for its time would push it late and the stream ahead of real time. Once
/// it is released it goes through here as one block, padded first only if still behind.
fn silence_before(real: u64, emitted: u64, block: u64, holding: bool) -> usize {
    let behind = real.saturating_sub(emitted + block);
    if !holding && behind > PAD_AFTER_MS * RATE / 1000 { behind as usize } else { 0 }
}

/// Whether two readable rates differ.
fn rate_changed(settled: Option<u32>, now: Option<u32>) -> bool {
    matches!((settled, now), (Some(a), Some(b)) if a != b)
}

/// A running process tap + private aggregate device + IO proc. Torn down on drop.
struct Tap {
    tap: AudioObjectID,
    agg: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    ctx: *mut Ctx,
    /// The rate audio arrives at (the device's, else the tap's).
    rate: u32,
    #[cfg(test)]
    test_rates: Option<(Option<u32>, Option<u32>)>,
    /// The process objects tapped (or, with `exclude`, the ones left out).
    objects: Vec<AudioObjectID>,
    exclude: bool,
}

// SAFETY: a `Tap` owns its Core Audio objects (plain ids, usable from any thread) and its `Ctx`,
// whose shared fields are atomics; the IO thread only reads it through the pointer handed to Core
// Audio, which `Drop` stops synchronously before freeing it. Moving the owner to another thread (the
// Teams-only tap is opened off the worker thread) changes none of that.
unsafe impl Send for Tap {}

impl Tap {
    /// An unreadable `DeviceIsAlive` (the aggregate was destroyed or invalidated) counts as dead.
    fn device_alive(&self) -> bool {
        is_alive(get::<u32>(self.agg, kAudioDevicePropertyDeviceIsAlive, None))
    }

    /// Whether any process this tap listens to is playing.
    fn sources_playing(&self) -> bool {
        if self.exclude {
            get_u32_list(kAudioObjectSystemObject as u32, kAudioHardwarePropertyProcessObjectList)
                .into_iter()
                .filter(|o| !self.objects.contains(o))
                .any(process_playing)
        } else {
            self.objects.iter().copied().any(process_playing)
        }
    }

    fn ctx(&self) -> Option<&Ctx> {
        unsafe { self.ctx.as_ref() }
    }

    /// Drain up to the next format boundary; true when it stopped at one.
    fn drain_into(&self, out: &mut Vec<f32>) -> bool {
        self.ctx().is_some_and(|c| c.ring.drain_into(out))
    }

    fn boundaries(&self) -> u64 {
        self.ctx().map_or(0, |c| c.boundaries.load(Ordering::Relaxed))
    }

    fn device_rate(&self) -> Option<u32> {
        #[cfg(test)]
        if let Some((device, _)) = self.test_rates {
            return device;
        }
        device_rate(self.agg)
    }

    fn tap_rate(&self) -> Option<u32> {
        #[cfg(test)]
        if let Some((_, tap)) = self.test_rates {
            return tap;
        }
        tap_rate(self.tap)
    }

    /// The rate audio arrives at now (the device's, else the tap's), as at open.
    fn rate_now(&self) -> Option<u32> {
        self.device_rate().or_else(|| self.tap_rate())
    }

    /// The rate the IO timestamps implied most recently (unknown right after a timeline jump).
    fn implied_rate(&self) -> Option<f64> {
        self.ctx().map(|c| f64::from_bits(c.implied_rate.load(Ordering::Relaxed))).filter(|r| *r > 0.0)
    }

    /// After a boundary: resample at `rate` from now on, and expect the IO timestamps to imply what
    /// they imply now (so a device whose timestamps run slightly off doesn't flag every cycle).
    fn settle(&mut self, rate: u32) {
        self.rate = rate;
        let expect = self.implied_rate().map_or(rate, |r| r.round() as u32);
        if let Some(c) = self.ctx() {
            c.expected_rate.store(expect, Ordering::Relaxed);
        }
    }

    fn callbacks(&self) -> u64 {
        self.ctx().map_or(0, |c| c.callbacks.load(Ordering::Relaxed))
    }

    /// Throw away what the IO thread queued so far (another capture already has it).
    fn discard_queued(&self) {
        if let Some(c) = self.ctx() {
            c.ring.discard();
        }
    }

    /// Whether the effective IO rate moved since open or the last boundary settlement. The
    /// aggregate's rate takes precedence because the tap format may catch up later.
    fn format_changed(&self) -> bool {
        rate_changed(Some(self.rate), self.rate_now())
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            if self.proc_id.is_some() {
                // Synchronous: the IO proc is not running and won't run again once these return.
                let _ = AudioDeviceStop(self.agg, self.proc_id);
                let _ = AudioDeviceDestroyIOProcID(self.agg, self.proc_id);
            }
            if self.agg != kAudioObjectUnknown {
                let _ = AudioHardwareDestroyAggregateDevice(self.agg);
            }
            if self.tap != kAudioObjectUnknown {
                let _ = AudioHardwareDestroyProcessTap(self.tap);
            }
            if !self.ctx.is_null() {
                drop(Box::from_raw(self.ctx));
            }
        }
    }
}

fn key(k: &CStr) -> Retained<NSString> {
    NSString::from_str(k.to_str().unwrap_or_default())
}

fn any(o: &NSObject) -> &AnyObject {
    o
}

/// Tap `objects` (or, with `exclude`, every process except them) and start receiving audio, unless
/// `stop` is set by then: creating the tap can block (the first one waits on the permission prompt),
/// and a tap that finishes opening after Stop must never start capturing.
fn open_tap(objects: &[AudioObjectID], exclude: bool, stop: &AtomicBool) -> anyhow::Result<Tap> {
    let mut tap = Tap {
        tap: kAudioObjectUnknown,
        agg: kAudioObjectUnknown,
        proc_id: None,
        ctx: std::ptr::null_mut(),
        rate: 0,
        #[cfg(test)]
        test_rates: None,
        objects: objects.to_vec(),
        exclude,
    };

    let uuid = objc2::rc::autoreleasepool(|_| -> anyhow::Result<String> {
        let nums: Vec<Retained<NSNumber>> = objects.iter().map(|&o| NSNumber::new_u32(o)).collect();
        let list = NSArray::from_retained_slice(&nums);
        let desc = unsafe {
            if exclude {
                CATapDescription::initMonoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &list)
            } else {
                CATapDescription::initMonoMixdownOfProcesses(CATapDescription::alloc(), &list)
            }
        };
        unsafe {
            desc.setName(&NSString::from_str("Wren meeting audio"));
            desc.setPrivate(true);
            desc.setMuteBehavior(CATapMuteBehavior::Unmuted);
        }
        let uuid = unsafe { desc.UUID() }.UUIDString().to_string();
        status("AudioHardwareCreateProcessTap", unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap.tap) })?;
        Ok(uuid)
    })?;

    let format: AudioStreamBasicDescription = get(tap.tap, kAudioTapPropertyFormat, None)
        .ok_or_else(|| anyhow::anyhow!("tap has no readable format"))?;
    if format.mFormatFlags & kAudioFormatFlagIsFloat == 0 || format.mBitsPerChannel != 32 || format.mSampleRate < 8_000.0 {
        anyhow::bail!("unexpected tap format: {} Hz, {} bits, flags {:#x}", format.mSampleRate, format.mBitsPerChannel, format.mFormatFlags);
    }

    objc2::rc::autoreleasepool(|_| -> anyhow::Result<()> {
        let tap_uid = NSString::from_str(&uuid);
        let yes = NSNumber::new_bool(true);
        let no = NSNumber::new_bool(false);
        let entry = NSDictionary::from_slices(
            &[&*key(kAudioSubTapUIDKey), &*key(kAudioSubTapDriftCompensationKey)],
            &[any(&tap_uid), any(&yes)],
        );
        let taps = NSArray::from_retained_slice(&[entry]);
        let agg_uid = NSString::from_str(&format!("uk.wren.meeting-tap.{}", uuid::Uuid::new_v4()));
        let name = NSString::from_str("Wren meeting audio");
        let desc = NSDictionary::from_slices(
            &[
                &*key(kAudioAggregateDeviceNameKey),
                &*key(kAudioAggregateDeviceUIDKey),
                &*key(kAudioAggregateDeviceIsPrivateKey),
                &*key(kAudioAggregateDeviceIsStackedKey),
                &*key(kAudioAggregateDeviceTapAutoStartKey),
                &*key(kAudioAggregateDeviceTapListKey),
            ],
            &[any(&name), any(&agg_uid), any(&yes), any(&no), any(&yes), any(&taps)],
        );
        // NSDictionary is toll-free bridged with CFDictionary.
        let cf: &CFDictionary = unsafe { &*(Retained::as_ptr(&desc) as *const CFDictionary) };
        status("AudioHardwareCreateAggregateDevice", unsafe {
            AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut tap.agg))
        })
    })?;

    tap.rate = tap
        .rate_now()
        .ok_or_else(|| anyhow::anyhow!("tap has no usable sample rate"))?;
    tap.ctx = Box::into_raw(Box::new(Ctx::new(tap.rate)));
    // `extern "C"` and `extern "C-unwind"` share the calling convention; only unwinding differs, and
    // `io_proc` never unwinds (a panic aborts at its boundary).
    let proc: AudioDeviceIOProc = Some(unsafe { std::mem::transmute::<IoProc, IoProcUnwind>(io_proc as IoProc) });
    status("AudioDeviceCreateIOProcID", unsafe {
        AudioDeviceCreateIOProcID(tap.agg, proc, tap.ctx.cast(), NonNull::from(&mut tap.proc_id))
    })?;
    // Last moment before IO starts: a Stop that came in while the tap was opening wins (the tap is
    // torn down by its Drop without ever having captured).
    if stop.load(Ordering::SeqCst) {
        anyhow::bail!("{STOPPED}");
    }
    status("AudioDeviceStart", unsafe { AudioDeviceStart(tap.agg, tap.proc_id) })?;
    Ok(tap)
}

/// What the Teams-attach decision was based on; a change triggers a reconnect.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Seen {
    teams_running: bool,
    teams: Vec<AudioObjectID>,
    own: Option<AudioObjectID>,
}

fn look() -> Seen {
    let pids = running_teams_pids();
    let procs = audio_processes();
    let teams = teams_objects(&procs, &pids);
    Seen { teams_running: !pids.is_empty() || !teams.is_empty(), teams, own: own_process_object() }
}

impl Seen {
    /// Whether what matters for `target` in `mode` differs from `before`.
    fn changed_from(&self, before: &Seen, target: Target, mode: CaptureMode) -> bool {
        match target {
            Target::Teams => {
                self.teams_running != before.teams_running
                    || self.teams != before.teams
                    || (mode == CaptureMode::AllSound && self.own != before.own)
            }
            Target::AllSound => self.own != before.own,
        }
    }
}

fn all_sound(seen: &Seen, loud: bool, stop: &AtomicBool) -> (Option<Tap>, CaptureMode) {
    let exclude: Vec<AudioObjectID> = seen.own.into_iter().collect();
    match open_tap(&exclude, true, stop) {
        Ok(t) => (Some(t), CaptureMode::AllSound),
        Err(_) if stop.load(Ordering::SeqCst) => (None, CaptureMode::MicOnly),
        Err(e) => {
            if loud {
                tracing::warn!(error = %e, "all-sound tap failed; recording the mic only");
            } else {
                tracing::debug!(error = %e, "all-sound tap failed again");
            }
            (None, CaptureMode::MicOnly)
        }
    }
}

/// Open the capture for `target`. `loud` logs failures as warnings (retries log quietly). Opens
/// nothing once `stop` is set.
fn connect(target: Target, seen: &Seen, loud: bool, stop: &AtomicBool) -> (Option<Tap>, CaptureMode) {
    match target {
        _ if stop.load(Ordering::SeqCst) => (None, CaptureMode::MicOnly),
        Target::AllSound => all_sound(seen, loud, stop),
        Target::Teams if !seen.teams_running => (None, CaptureMode::MicOnly),
        // Teams is running but hasn't opened audio yet: nothing to tap until it does (rechecked).
        Target::Teams if seen.teams.is_empty() => (None, CaptureMode::TeamsOnly),
        Target::Teams => match open_tap(&seen.teams, false, stop) {
            Ok(t) => (Some(t), CaptureMode::TeamsOnly),
            Err(_) if stop.load(Ordering::SeqCst) => (None, CaptureMode::MicOnly),
            Err(e) => {
                if loud {
                    tracing::warn!(error = %e, "Teams tap failed; capturing all computer sound");
                } else {
                    tracing::debug!(error = %e, "Teams tap failed again");
                }
                all_sound(seen, loud, stop)
            }
        },
    }
}

/// The state a recheck decides on (pure input to [`decide`]).
#[derive(Debug, Clone, Copy)]
struct Check {
    target: Target,
    mode: CaptureMode,
    has_tap: bool,
    /// No IO callback for longer than [`STALL`].
    stalled: bool,
    /// The effective IO rate differs from the active resampler's rate.
    format_changed: bool,
    /// The rate after a format boundary couldn't be settled (see [`AtBoundary::Unknown`]).
    rate_unknown: bool,
    /// Teams appeared, quit, restarted or changed its audio processes (see [`Seen::changed_from`]).
    seen_changed: bool,
    teams_running: bool,
    /// Teams has audio process objects to tap.
    teams_has_audio: bool,
    /// [`UPGRADE_RETRY`] passed since the last Teams-only attempt.
    upgrade_due: bool,
    /// A Teams-only tap is already being opened, or is open and waiting to deliver.
    upgrade_busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Keep,
    /// Tear the capture down and connect again from scratch.
    Reconnect,
    /// On the all-sound fallback: start opening a Teams-only tap in the background; the fallback
    /// keeps running and is replaced only once the new tap delivers (see [`Upgrade`]).
    TryTeams,
}

fn decide(c: &Check) -> Action {
    if c.seen_changed || (c.has_tap && (c.stalled || c.format_changed || c.rate_unknown)) {
        return Action::Reconnect;
    }
    match c.target {
        Target::AllSound if !c.has_tap => Action::Reconnect,
        Target::AllSound => Action::Keep,
        // Both taps failed (or the tap was lost) while Teams has audio: try again.
        Target::Teams if !c.has_tap && c.teams_running && c.teams_has_audio => Action::Reconnect,
        Target::Teams
            if c.mode == CaptureMode::AllSound && c.has_tap && c.teams_has_audio && c.upgrade_due && !c.upgrade_busy =>
        {
            Action::TryTeams
        }
        Target::Teams => Action::Keep,
    }
}

/// A capture that can replace the current one once it is known to deliver.
trait Candidate {
    fn delivered(&self) -> bool;
}

impl Candidate for Tap {
    /// The IO proc has run: the tap is live (a tap on processes that aren't playing never calls back).
    fn delivered(&self) -> bool {
        self.callbacks() > 0
    }
}

/// The Teams-only retry while on the all-sound fallback. Opening a tap can block inside Core Audio, so
/// it happens on its own thread; the worker only ever polls ([`Upgrade::poll`] never blocks), so the
/// fallback's ring keeps being drained and emitted throughout. The new tap replaces the fallback only
/// once it delivers.
enum Upgrade<T> {
    Idle,
    Opening(crossbeam_channel::Receiver<anyhow::Result<T>>),
    /// Opened; waiting for its first IO callback.
    Waiting(T),
}

/// What one [`Upgrade::poll`] found.
enum Polled<T> {
    Nothing,
    Failed(anyhow::Error),
    /// Open and delivering: switch to it.
    Ready(T),
}

impl<T: Candidate> Upgrade<T> {
    fn busy(&self) -> bool {
        !matches!(self, Upgrade::Idle)
    }

    fn poll(&mut self) -> Polled<T> {
        use crossbeam_channel::TryRecvError;
        match std::mem::replace(self, Upgrade::Idle) {
            Upgrade::Idle => Polled::Nothing,
            Upgrade::Opening(rx) => match rx.try_recv() {
                Ok(Ok(t)) => {
                    *self = Upgrade::Waiting(t);
                    self.poll()
                }
                Ok(Err(e)) => Polled::Failed(e),
                Err(TryRecvError::Empty) => {
                    *self = Upgrade::Opening(rx);
                    Polled::Nothing
                }
                Err(TryRecvError::Disconnected) => Polled::Failed(anyhow::anyhow!("the Teams tap opener ended without a result")),
            },
            Upgrade::Waiting(t) if t.delivered() => Polled::Ready(t),
            waiting => {
                *self = waiting;
                Polled::Nothing
            }
        }
    }
}

/// Open a Teams-only tap on `objects` on its own thread. A tap that opens after the worker has gone
/// is dropped (torn down) by the opener; one that opens after Stop never starts (see [`open_tap`]).
fn open_in_background(objects: Vec<AudioObjectID>, stop: Arc<AtomicBool>) -> Upgrade<Tap> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    // If the thread can't be spawned, `tx` is dropped with the closure and the poll reports it.
    let _ = std::thread::Builder::new().name("wren-teams-tap-open".into()).spawn(move || {
        let _ = tx.send(open_tap(&objects, false, &stop));
    });
    Upgrade::Opening(rx)
}

fn resampler(rate: u32) -> Option<Resampler16k> {
    match Resampler16k::new(rate) {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::warn!(error = %e, rate, "cannot resample Teams audio");
            None
        }
    }
}

/// The current capture: tap (if any), its resampler and liveness bookkeeping.
struct Conn {
    tap: Option<Tap>,
    mode: CaptureMode,
    resampler: Option<Resampler16k>,
    callbacks: u64,
    last_io: Instant,
    /// Stopped at a format boundary (since then): nothing after it is resampled until its rate is known.
    boundary: Option<Instant>,
    /// The rate after a boundary couldn't be settled ([`AtBoundary::Unknown`]): reopen the tap.
    rate_unknown: bool,
    /// Whether an unsettled rate may ask for a reopen (the worker clears it once [`ReopenBudget`] is
    /// spent; the property then decides).
    may_reopen: bool,
}

impl Conn {
    fn new(tap: Option<Tap>, mode: CaptureMode) -> Self {
        let resampler = tap.as_ref().and_then(|t| resampler(t.rate));
        Self { tap, mode, resampler, callbacks: 0, last_io: Instant::now(), boundary: None, rate_unknown: false, may_reopen: true }
    }

    /// Audio is held back at a format boundary (it will still be emitted): no wall-clock padding.
    fn holding(&self) -> bool {
        self.boundary.is_some()
    }

    fn pump(&mut self, raw: &mut Vec<f32>, out: &mut Vec<f32>) {
        self.drain(raw, out, false);
    }

    /// Move the queued IO through the resampler. At a boundary a new rate flushes the old
    /// resampler and starts one at the new rate. A final drain resolves every queued boundary now.
    fn drain(&mut self, raw: &mut Vec<f32>, out: &mut Vec<f32>, finishing: bool) {
        let Some(t) = self.tap.as_mut() else { return };
        let n = t.callbacks();
        if n != self.callbacks {
            self.callbacks = n;
            self.last_io = Instant::now();
        }
        // Each pass ends at a boundary or at the end of what has arrived; a few is plenty per tick.
        for _ in 0..4 {
            if let Some(since) = self.boundary {
                let (prop, implied) = (t.rate_now(), t.implied_rate());
                let timed_out = finishing || since.elapsed() >= BOUNDARY_HOLD;
                let decided = match at_boundary(t.rate, prop, implied, timed_out) {
                    AtBoundary::Unknown if self.may_reopen && !finishing => {
                        // The worker will flush the held samples before reopening the tap.
                        self.rate_unknown = true;
                        return;
                    }
                    AtBoundary::Unknown => {
                        tracing::warn!(
                            rate = t.rate,
                            prop = ?prop,
                            implied = ?implied.map(|r| r.round() as u32),
                            finishing,
                            "Teams audio rate still unclear; using the reported rate or the last settled rate"
                        );
                        fallback_rate(t.rate, prop)
                    }
                    d => d,
                };
                match decided {
                    AtBoundary::Wait | AtBoundary::Unknown => return,
                    AtBoundary::Same => t.settle(t.rate),
                    AtBoundary::Retune(rate) => {
                        tracing::info!(
                            from = t.rate,
                            to = rate,
                            boundaries = t.boundaries(),
                            "Teams audio rate changed; resampling at the new rate"
                        );
                        if let Some(r) = self.resampler.as_mut() {
                            let _ = r.finish(out);
                        }
                        self.resampler = resampler(rate);
                        t.settle(rate);
                    }
                }
                self.boundary = None;
            }
            raw.clear();
            let at_boundary = t.drain_into(raw);
            if let Some(r) = self.resampler.as_mut() {
                let _ = r.push(raw, out);
            }
            if !at_boundary {
                return;
            }
            self.boundary = Some(Instant::now());
        }
    }

    /// Drain the last queued audio, including any held at a boundary, then flush the resampler.
    fn finish(&mut self, raw: &mut Vec<f32>, out: &mut Vec<f32>) {
        self.drain(raw, out, true);
        if let Some(r) = self.resampler.as_mut() {
            let _ = r.finish(out);
        }
    }

    fn health(&self) -> (bool, bool) {
        if self.holding() {
            // The boundary resolver owns recovery until the held audio has a rate or times out.
            return (false, false);
        }
        (self.tap.as_ref().is_some_and(Tap::format_changed), self.stalled())
    }

    fn try_upgrade(&mut self, mut tap: Tap, raw: &mut Vec<f32>, out: &mut Vec<f32>) -> bool {
        // The candidate may have been waiting through a rate change. Stay on the working fallback
        // if the candidate's current rate is unreadable or disagrees with its IO timestamps.
        let Some(rate) = tap.rate_now() else { return false };
        if tap.implied_rate().is_some_and(|r| off_rate(r, f64::from(rate))) {
            return false;
        }
        tap.settle(rate);
        // The fallback heard Teams too. Flush it and discard the overlapping candidate audio.
        self.finish(raw, out);
        tap.discard_queued();
        *self = Self::new(Some(tap), CaptureMode::TeamsOnly);
        true
    }

    /// Checks Core Audio only once callbacks have been missing for [`STALL`].
    fn stalled(&self) -> bool {
        let Some(t) = &self.tap else { return false };
        let silent_for = self.last_io.elapsed();
        silent_for > STALL && is_stalled(silent_for, t.device_alive(), t.sources_playing())
    }
}

pub struct TeamsTap {
    target: Target,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl TeamsTap {
    pub fn new(target: Target) -> Self {
        Self { target, stop: Arc::new(AtomicBool::new(false)), thread: None }
    }
}

impl AudioSource for TeamsTap {
    fn start(&mut self, tx: Sender<AudioBlock>, mode_tx: Sender<CaptureMode>) -> anyhow::Result<CaptureMode> {
        let (first_tx, first_rx) = crossbeam_channel::bounded(1);
        // Set when `start` gave up waiting (e.g. the first tap waits on the permission prompt): the
        // thread then reports its first mode as a change instead.
        let late = Arc::new(parking_lot::Mutex::new(false));
        let late_thread = late.clone();
        let stop = self.stop.clone();
        let target = self.target;
        let thread = std::thread::Builder::new().name("wren-teams-audio".into()).spawn(move || {
            let started = Instant::now();
            let mut seen = look();
            let (tap, mode) = connect(target, &seen, true, &stop);
            let mut conn = Conn::new(tap, mode);
            {
                let late = late_thread.lock();
                if *late {
                    let _ = mode_tx.send(mode);
                } else {
                    let _ = first_tx.send(mode);
                }
            }
            let mut emitted: u64 = 0; // 16 kHz samples
            let mut raw: Vec<f32> = Vec::new();
            let mut out: Vec<f32> = Vec::new();
            let mut last_check = Instant::now();
            let mut last_health = Instant::now();
            let mut last_upgrade = Instant::now();
            let mut last_loud = Instant::now();
            let mut upgrade: Upgrade<Tap> = Upgrade::Idle;
            let mut rate_reopens = ReopenBudget::default();
            let send = |samples: Vec<f32>, emitted: &mut u64| {
                let pos = *emitted * 1000 / RATE;
                *emitted += samples.len() as u64;
                tx.send(AudioBlock { samples, stream_pos_ms: pos }).is_ok()
            };
            // Audio that arrives after a stall (slow open, reconnect) keeps its real time: the gap
            // before it is filled with silence first, as in `mic.rs`. Not while audio is held at a
            // format boundary (`holding`): it is still to come and fills that time itself.
            let emit = |samples: Vec<f32>, emitted: &mut u64, holding: bool| {
                let real = started.elapsed().as_millis() as u64 * RATE / 1000;
                let gap = silence_before(real, *emitted, samples.len() as u64, holding);
                (gap == 0 || send(vec![0.0; gap], emitted)) && send(samples, emitted)
            };
            'run: while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                conn.may_reopen = rate_reopens.allowed(Instant::now());
                conn.pump(&mut raw, &mut out);
                if !out.is_empty() && !emit(std::mem::take(&mut out), &mut emitted, conn.holding()) {
                    break 'run;
                }
                // Keep up with real time when nothing (or too little) arrives.
                let real = started.elapsed().as_millis() as u64 * RATE / 1000;
                let gap = silence_before(real, emitted, 0, conn.holding());
                if gap > 0 && !send(vec![0.0; gap], &mut emitted) {
                    break 'run;
                }

                // Poll the Teams-only retry without waiting for the opener. Give the fallback's
                // boundary time to settle before switching to the candidate.
                let polled = if conn.holding() { Polled::Nothing } else { upgrade.poll() };
                match polled {
                    Polled::Nothing => {}
                    Polled::Failed(e) => {
                        last_upgrade = Instant::now();
                        if !stop.load(Ordering::SeqCst) {
                            tracing::debug!(error = %e, "Teams tap still failing; staying on all sound");
                        }
                    }
                    Polled::Ready(t) if target == Target::Teams && conn.mode == CaptureMode::AllSound => {
                        if !conn.try_upgrade(t, &mut raw, &mut out) {
                            last_upgrade = Instant::now();
                            tracing::debug!("Teams tap rate is unsettled; staying on all sound");
                            continue;
                        }
                        tracing::info!("Teams tap delivering; leaving the all-sound fallback");
                        let _ = mode_tx.send(conn.mode);
                        if !out.is_empty() && !emit(std::mem::take(&mut out), &mut emitted, false) {
                            break 'run;
                        }
                    }
                    Polled::Ready(t) => drop(t),
                }

                let (format_changed, stalled) = if last_health.elapsed() >= HEALTH_CHECK {
                    last_health = Instant::now();
                    conn.health()
                } else {
                    (false, false)
                };
                let rate_unknown = conn.rate_unknown;
                if !(format_changed || stalled || rate_unknown || last_check.elapsed() >= RECHECK) {
                    continue;
                }
                last_check = Instant::now();
                let now = look();
                let check = Check {
                    target,
                    mode: conn.mode,
                    has_tap: conn.tap.is_some(),
                    stalled,
                    format_changed,
                    rate_unknown,
                    seen_changed: now.changed_from(&seen, target, conn.mode),
                    teams_running: now.teams_running,
                    teams_has_audio: !now.teams.is_empty(),
                    upgrade_due: last_upgrade.elapsed() >= UPGRADE_RETRY,
                    upgrade_busy: upgrade.busy(),
                };
                let next = match decide(&check) {
                    Action::Keep => None,
                    Action::Reconnect => {
                        // Retries that keep happening (both taps failing, a tap that keeps dying)
                        // log at most once a minute; Teams starting or quitting always logs.
                        let loud = check.seen_changed || last_loud.elapsed() >= QUIET_RETRY_LOGS;
                        if rate_unknown {
                            let attempt = rate_reopens.spend(Instant::now());
                            tracing::info!(
                                attempt,
                                of = RATE_REOPENS,
                                "Teams audio rate unclear after a format change; reopening the tap"
                            );
                        }
                        if loud {
                            last_loud = Instant::now();
                            tracing::info!(
                                stalled,
                                format_changed,
                                seen_changed = check.seen_changed,
                                had_tap = check.has_tap,
                                "reconnecting Teams audio"
                            );
                        }
                        conn.finish(&mut raw, &mut out);
                        // Emit before opening the replacement, so a slow reconnect can't put its
                        // gap before these samples.
                        if !out.is_empty() && !emit(std::mem::take(&mut out), &mut emitted, false) {
                            break 'run;
                        }
                        drop(conn.tap.take());
                        // A pending Teams-only retry was for the capture being replaced.
                        upgrade = Upgrade::Idle;
                        let (t, m) = connect(target, &now, loud, &stop);
                        if m == CaptureMode::AllSound && conn.mode != CaptureMode::AllSound {
                            last_upgrade = Instant::now();
                        }
                        Some(Conn::new(t, m))
                    }
                    Action::TryTeams => {
                        last_upgrade = Instant::now();
                        upgrade = open_in_background(now.teams.clone(), stop.clone());
                        None
                    }
                };
                seen = now;
                if let Some(next) = next {
                    let previous = std::mem::replace(&mut conn, next);
                    drop(previous);
                    if conn.mode != check.mode {
                        let _ = mode_tx.send(conn.mode);
                    }
                }
            }
            if stop.load(Ordering::Relaxed) {
                conn.finish(&mut raw, &mut out);
                if !out.is_empty() {
                    emit(out, &mut emitted, false);
                }
            }
        })?;
        self.thread = Some(thread);
        let mode = match first_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(m) => m,
            Err(_) => {
                let mut late = late.lock();
                first_rx.try_recv().unwrap_or_else(|_| {
                    *late = true;
                    CaptureMode::MicOnly
                })
            }
        };
        Ok(mode)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            // Creating a tap can block inside Core Audio (the permission prompt, a wedged coreaudiod):
            // never let that hang Stop or quitting. Wait briefly, then leave the thread to finish on
            // its own; it checks the flag before starting IO, so it never starts capturing after this.
            let until = Instant::now() + STOP_JOIN;
            while !t.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            if t.is_finished() {
                let _ = t.join();
            } else {
                tracing::warn!("Teams audio capture didn't stop in time; leaving it to finish on its own");
            }
        }
    }
}

impl Drop for TeamsTap {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- permission ----

/// The "System Audio Recording" permission for this app: `"granted"`, `"denied"` or `"unknown"`
/// (never asked, or the private TCC preflight isn't available on this macOS).
pub fn system_audio_permission() -> &'static str {
    type Preflight = unsafe extern "C" fn(service: *const c_void, options: *const c_void) -> i32;
    static PREFLIGHT: std::sync::OnceLock<Option<Preflight>> = std::sync::OnceLock::new();
    let f = PREFLIGHT.get_or_init(|| unsafe {
        let lib = libc::dlopen(c"/System/Library/PrivateFrameworks/TCC.framework/Versions/A/TCC".as_ptr(), libc::RTLD_NOW);
        if lib.is_null() {
            return None;
        }
        let sym = libc::dlsym(lib, c"TCCAccessPreflight".as_ptr());
        (!sym.is_null()).then(|| std::mem::transmute::<*mut c_void, Preflight>(sym))
    });
    let Some(f) = f else { return "unknown" };
    let service = NSString::from_str("kTCCServiceAudioCapture");
    match unsafe { f(Retained::as_ptr(&service).cast(), std::ptr::null()) } {
        0 => "granted",
        1 => "denied",
        _ => "unknown",
    }
}

/// Open System Settings › Privacy & Security › Screen & System Audio Recording.
pub fn open_system_audio_settings() {
    if let Err(e) = std::process::Command::new("/usr/bin/open").arg(SYSTEM_AUDIO_SETTINGS_URI).spawn() {
        tracing::warn!(error = %e, "could not open System Audio Recording settings");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stop flag that is never set.
    static RUN: AtomicBool = AtomicBool::new(false);

    #[test]
    fn ring_stops_at_a_boundary_and_consumes_it() {
        let ring = Ring::new(16);
        ring.push_with(3, |i| i as f32);
        ring.mark_boundary();
        ring.push_with(2, |i| 10.0 + i as f32);
        // A second boundary before the first is consumed is not kept.
        ring.mark_boundary();
        ring.push_with(1, |_| 20.0);
        let mut out = Vec::new();
        assert!(ring.drain_into(&mut out), "stopped at the boundary");
        assert_eq!(out, vec![0.0, 1.0, 2.0]);
        out.clear();
        assert!(!ring.drain_into(&mut out));
        assert_eq!(out, vec![10.0, 11.0, 20.0]);
    }

    #[test]
    fn ring_boundary_with_nothing_before_it() {
        let ring = Ring::new(16);
        ring.mark_boundary();
        ring.push_with(2, |i| i as f32);
        let mut out = Vec::new();
        assert!(ring.drain_into(&mut out));
        assert!(out.is_empty());
        assert!(!ring.drain_into(&mut out));
        assert_eq!(out, vec![0.0, 1.0]);
    }

    #[test]
    fn ring_boundary_not_yet_reached_is_kept() {
        let ring = Ring::new(16);
        ring.push_with(2, |i| i as f32);
        let mut out = Vec::new();
        assert!(!ring.drain_into(&mut out));
        ring.mark_boundary();
        ring.push_with(1, |_| 5.0);
        out.clear();
        assert!(ring.drain_into(&mut out));
        assert!(out.is_empty(), "nothing between the last drain and the boundary");
        assert!(!ring.drain_into(&mut out));
        assert_eq!(out, vec![5.0]);
    }

    const HZ: f64 = 24_000_000.0; // Apple silicon host clock

    fn cycle(sample: f64, secs: f64, frames: usize) -> Cycle {
        Cycle { sample, host: (secs * HZ) as u64, frames }
    }

    #[test]
    fn steps_find_jumps_and_implied_rates() {
        let a = cycle(0.0, 1.0, 480);
        // 480 frames in 10 ms: 48 kHz.
        let Step::Rate(r) = step(a, cycle(480.0, 1.010, 480), HZ) else { panic!() };
        assert!((r - 48_000.0).abs() < 1.0);
        // Same frames, 10.88 ms: 44.1 kHz.
        let Step::Rate(r) = step(a, cycle(480.0, 1.0 + 480.0 / 44_100.0, 480), HZ) else { panic!() };
        assert!(off_rate(r, 48_000.0) && !off_rate(r, 44_100.0));
        // IO restarted: the timeline starts over.
        assert_eq!(step(a, cycle(0.0, 1.5, 480), HZ), Step::Jump);
        assert_eq!(step(a, cycle(10_000.0, 1.2, 480), HZ), Step::Jump);
        // Host time didn't move: nothing to say.
        assert_eq!(step(a, cycle(480.0, 1.0, 480), HZ), Step::Unknown);
        assert!(!off_rate(48_100.0, 48_000.0), "clock drift is not a format change");
    }

    #[test]
    fn boundary_waits_for_property_and_timestamps_to_agree() {
        // Rate changed and the property already says so.
        assert_eq!(at_boundary(48_000, Some(24_000), Some(24_010.0), false), AtBoundary::Retune(24_000));
        // A jump (overload, restart) at the same rate.
        assert_eq!(at_boundary(48_000, Some(48_000), Some(47_990.0), false), AtBoundary::Same);
        // The timestamps already run at 24 kHz but the property still says 48: hold.
        assert_eq!(at_boundary(48_000, Some(48_000), Some(24_000.0), false), AtBoundary::Wait);
        // No cycle measured since the jump yet: hold.
        assert_eq!(at_boundary(48_000, Some(24_000), None, false), AtBoundary::Wait);
        // Property unreadable: the timestamps decide whether it's still the same rate.
        assert_eq!(at_boundary(48_000, None, Some(48_000.0), false), AtBoundary::Same);
        assert_eq!(at_boundary(48_000, None, Some(16_000.0), false), AtBoundary::Wait);
        // Held long enough with nothing measured since: the property is all there is.
        assert_eq!(at_boundary(48_000, Some(16_000), None, true), AtBoundary::Retune(16_000));
        assert_eq!(at_boundary(48_000, Some(48_000), None, true), AtBoundary::Same);
        // Agreement still settles it after the hold.
        assert_eq!(at_boundary(48_000, Some(24_000), Some(24_000.0), true), AtBoundary::Retune(24_000));
    }

    #[test]
    fn rate_that_stays_ambiguous_reopens_instead_of_guessing() {
        // Held long enough and the property and the timestamps still disagree: unknown, not a guess.
        assert_eq!(at_boundary(48_000, Some(48_000), Some(24_000.0), true), AtBoundary::Unknown);
        // Property unreadable and the timestamps say another rate, or say nothing.
        assert_eq!(at_boundary(48_000, None, Some(16_000.0), true), AtBoundary::Unknown);
        assert_eq!(at_boundary(48_000, None, None, true), AtBoundary::Unknown);
        // Before the hold is up it is still just a wait.
        assert_eq!(at_boundary(48_000, None, None, false), AtBoundary::Wait);
        // Reopen budget spent: the property decides, else the current rate.
        assert_eq!(fallback_rate(48_000, Some(24_000)), AtBoundary::Retune(24_000));
        assert_eq!(fallback_rate(48_000, None), AtBoundary::Same);
        // An unsettled rate reconnects (that is the reopen).
        let healthy = check(Target::Teams, CaptureMode::TeamsOnly, true);
        assert_eq!(decide(&Check { rate_unknown: true, ..healthy }), Action::Reconnect);
    }

    #[test]
    fn rate_reopens_are_bounded_per_window() {
        let t0 = Instant::now();
        let mut b = ReopenBudget::default();
        assert!(b.allowed(t0));
        assert_eq!(b.spend(t0), 1);
        assert_eq!(b.spend(t0 + Duration::from_secs(1)), 2);
        assert!(b.allowed(t0 + Duration::from_secs(2)));
        assert_eq!(b.spend(t0 + Duration::from_secs(2)), 3);
        assert!(!b.allowed(t0 + Duration::from_secs(3)), "three reopens in a minute: stop reopening");
        assert!(b.allowed(t0 + RATE_REOPEN_WINDOW), "a minute on, the budget is back");
        assert_eq!(b.spend(t0 + RATE_REOPEN_WINDOW), 1);
    }

    fn test_tap(rate: u32) -> Tap {
        Tap {
            tap: kAudioObjectUnknown,
            agg: kAudioObjectUnknown,
            proc_id: None,
            ctx: Box::into_raw(Box::new(Ctx::new(rate))),
            rate,
            test_rates: Some((Some(rate), Some(rate))),
            objects: Vec::new(),
            exclude: false,
        }
    }

    #[test]
    fn health_check_leaves_a_held_format_boundary_to_the_resolver() {
        let mut tap = test_tap(48_000);
        tap.test_rates = Some((Some(24_000), Some(48_000)));
        tap.ctx().unwrap().ring.mark_boundary();
        let mut conn = Conn::new(Some(tap), CaptureMode::TeamsOnly);
        conn.last_io -= STALL + TICK;
        conn.pump(&mut Vec::new(), &mut Vec::new());
        assert!(conn.holding());
        assert_eq!(conn.health(), (false, false));
        conn.boundary = None;
        assert_eq!(conn.health(), (true, true));
    }

    #[test]
    fn a_late_tap_format_does_not_reopen_a_settled_device_rate() {
        let mut tap = test_tap(48_000);
        tap.test_rates = Some((Some(24_000), Some(48_000)));
        assert!(tap.format_changed());
        tap.settle(24_000);
        assert!(!tap.format_changed());
        tap.test_rates = Some((Some(24_000), Some(24_000)));
        assert!(!tap.format_changed(), "the secondary rate caught up to the settled rate");
        tap.test_rates = Some((Some(48_000), Some(24_000)));
        assert!(tap.format_changed(), "the effective rate changed again");
        tap.test_rates = Some((None, Some(48_000)));
        assert!(tap.format_changed(), "the tap rate is used when the device rate is unreadable");
    }

    #[test]
    fn finish_flushes_audio_held_at_a_boundary_before_reconnect() {
        let mut tap = test_tap(48_000);
        tap.test_rates = Some((Some(16_000), Some(48_000)));
        let ring = &tap.ctx().unwrap().ring;
        ring.push_with(4_800, |_| 0.1);
        ring.mark_boundary();
        ring.push_with(1_600, |_| 0.3);
        let mut conn = Conn::new(Some(tap), CaptureMode::TeamsOnly);
        let (mut raw, mut out) = (Vec::new(), Vec::new());
        conn.pump(&mut raw, &mut out);
        assert!(conn.holding(), "the rate is waiting for IO timestamps");

        conn.finish(&mut raw, &mut out);

        assert_eq!(out.len(), 3_200, "100 ms on either side of the boundary");
        assert!(out[1_600..].iter().all(|&sample| sample == 0.3));
    }

    #[test]
    fn teams_upgrade_uses_the_rate_when_the_waiting_tap_starts_delivering() {
        let mut up = Upgrade::Waiting(test_tap(48_000));
        assert!(matches!(up.poll(), Polled::Nothing));
        let Upgrade::Waiting(tap) = &mut up else { panic!("expected the candidate to keep waiting") };
        tap.test_rates = Some((Some(16_000), Some(48_000)));
        tap.ctx().unwrap().callbacks.store(1, Ordering::Relaxed);
        let Polled::Ready(tap) = up.poll() else { panic!("expected the candidate to be ready") };
        let mut conn = Conn::new(None, CaptureMode::AllSound);
        let (mut raw, mut out) = (Vec::new(), Vec::new());
        assert!(conn.try_upgrade(tap, &mut raw, &mut out));
        assert_eq!(conn.mode, CaptureMode::TeamsOnly);
        let tap = conn.tap.as_ref().unwrap();
        assert_eq!(tap.rate, 16_000);
        assert_eq!(tap.ctx().unwrap().expected_rate.load(Ordering::Relaxed), 16_000);
        tap.ctx().unwrap().ring.push_with(1_600, |_| 0.3);
        conn.pump(&mut raw, &mut out);
        assert_eq!(out, vec![0.3; 1_600]);
    }

    #[test]
    fn resampler_restarts_at_a_rate_boundary() {
        // 0.5 s at 48 kHz, a boundary, 0.5 s at 24 kHz: 16 kHz output is 0.5 s + 0.5 s, not the
        // 0.25 s the 24 kHz half would shrink to through a 48 kHz resampler.
        let ring = Ring::new(65_536);
        ring.push_with(24_000, |_| 0.1);
        ring.mark_boundary();
        ring.push_with(12_000, |_| 0.1);
        let mut current = Resampler16k::new(48_000).unwrap();
        let (mut raw, mut out) = (Vec::new(), Vec::new());
        assert!(ring.drain_into(&mut raw));
        current.push(&raw, &mut out).unwrap();
        assert_eq!(at_boundary(48_000, Some(24_000), Some(24_000.0), false), AtBoundary::Retune(24_000));
        current.finish(&mut out).unwrap();
        let mut current = Resampler16k::new(24_000).unwrap();
        raw.clear();
        assert!(!ring.drain_into(&mut raw));
        current.push(&raw, &mut out).unwrap();
        current.finish(&mut out).unwrap();
        assert_eq!(out.len(), 16_000);
    }

    #[test]
    fn silence_goes_before_late_audio() {
        let pad = (PAD_AFTER_MS * RATE / 1000) as usize;
        // On time: nothing to pad.
        assert_eq!(silence_before(16_000, 14_400, 1_600, false), 0);
        // Slightly behind (under the threshold): nothing.
        assert_eq!(silence_before(16_000 + pad as u64, 14_400, 1_600, false), 0);
        // 2 s stall, then a 100 ms block: 2 s of silence first so the block ends at real time.
        assert_eq!(silence_before(48_000, 14_400, 1_600, false), 48_000 - 14_400 - 1_600);
        // Nothing arrived: pad up to now.
        assert_eq!(silence_before(48_000, 14_400, 0, false), 48_000 - 14_400);
        // Ahead of real time (a burst): never negative.
        assert_eq!(silence_before(1_000, 5_000, 1_600, false), 0);
    }

    #[test]
    fn no_padding_while_audio_is_held_at_a_boundary() {
        // 1 s held at a rate boundary: the stream is 1 s behind, but that second is still coming.
        assert_eq!(silence_before(32_000, 16_000, 0, true), 0);
        assert_eq!(silence_before(32_000, 14_400, 1_600, true), 0, "nor before pre-boundary audio");
        // Released: the held second goes out as one block and lands on real time with no silence first.
        assert_eq!(silence_before(32_000, 16_000, 16_000, false), 0);
        // Released after a longer stall than it covers: pad only what is still missing, before it.
        assert_eq!(silence_before(64_000, 16_000, 16_000, false), 32_000);
    }

    #[test]
    fn ring_holds_two_seconds_of_48k_even_when_opened_at_hands_free_rates() {
        for rate in [8_000, 16_000, 24_000, 44_100, 48_000, 96_000] {
            let cap = Ring::new(ring_samples(rate)).buf.len();
            assert!(cap >= 48_000 * RING_SECS, "{rate} Hz: {cap}");
            assert!(cap >= rate as usize * RING_SECS, "{rate} Hz: {cap}");
        }
        // A 16 kHz tap that switches to 48 kHz and is held for the whole boundary hold loses nothing.
        let ring = Ring::new(ring_samples(16_000));
        ring.push_with(1_600, |_| 0.0); // 100 ms at 16 kHz
        ring.mark_boundary();
        let held = 48_000 * BOUNDARY_HOLD.as_millis() as usize / 1000 + 4_800; // the hold plus a tick
        ring.push_with(held, |_| 0.1);
        assert_eq!(ring.dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ring_discard_keeps_a_boundary_still_ahead() {
        let ring = Ring::new(16);
        ring.push_with(3, |i| i as f32);
        ring.mark_boundary();
        ring.push_with(2, |_| 9.0);
        // A boundary inside what is thrown away goes with it.
        ring.discard();
        let mut out = Vec::new();
        assert!(!ring.drain_into(&mut out));
        assert!(out.is_empty());
        // A boundary at the write position (nothing after it yet) still applies to what comes next.
        ring.push_with(2, |_| 1.0);
        ring.mark_boundary();
        ring.discard();
        ring.push_with(1, |_| 5.0);
        assert!(ring.drain_into(&mut out), "boundary kept");
        assert!(out.is_empty());
        assert!(!ring.drain_into(&mut out));
        assert_eq!(out, vec![5.0]);
    }

    /// A fake Teams-only tap: delivers once `calls` is set.
    struct Fake(Arc<AtomicU64>);
    impl Candidate for Fake {
        fn delivered(&self) -> bool {
            self.0.load(Ordering::SeqCst) > 0
        }
    }

    #[test]
    fn teams_retry_never_blocks_and_switches_only_once_the_tap_delivers() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut up: Upgrade<Fake> = Upgrade::Opening(rx);
        // Still opening (Core Audio blocking on the opener thread): the poll returns at once.
        let t0 = Instant::now();
        assert!(matches!(up.poll(), Polled::Nothing));
        assert!(t0.elapsed() < Duration::from_millis(50));
        assert!(up.busy());
        // Opened, but no IO callback yet: keep the fallback.
        let calls = Arc::new(AtomicU64::new(0));
        tx.send(Ok(Fake(calls.clone()))).unwrap();
        assert!(matches!(up.poll(), Polled::Nothing));
        assert!(matches!(up, Upgrade::Waiting(_)));
        assert!(matches!(up.poll(), Polled::Nothing));
        // Delivering: switch.
        calls.store(1, Ordering::SeqCst);
        assert!(matches!(up.poll(), Polled::Ready(_)));
        assert!(!up.busy());
        // A failed open is reported once, then idle (the next try waits for UPGRADE_RETRY).
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut up: Upgrade<Fake> = Upgrade::Opening(rx);
        tx.send(Err(anyhow::anyhow!("no"))).unwrap();
        assert!(matches!(up.poll(), Polled::Failed(_)));
        assert!(matches!(up.poll(), Polled::Nothing) && !up.busy());
        // The opener thread died without a result.
        let (tx, rx) = crossbeam_channel::bounded::<anyhow::Result<Fake>>(1);
        drop(tx);
        let mut up = Upgrade::Opening(rx);
        assert!(matches!(up.poll(), Polled::Failed(_)));
    }

    #[test]
    fn unreadable_device_is_dead() {
        assert!(is_alive(Some(1)));
        assert!(!is_alive(Some(0)));
        assert!(!is_alive(None), "an error reading DeviceIsAlive means the aggregate is gone");
    }

    #[test]
    fn stop_is_bounded_when_the_worker_is_stuck() {
        let stop = Arc::new(AtomicBool::new(false));
        // A worker stuck in Core Audio (e.g. tap creation waiting on the permission prompt).
        let stuck = std::thread::spawn(|| std::thread::sleep(Duration::from_secs(6)));
        let mut tap = TeamsTap { target: Target::Teams, stop: stop.clone(), thread: Some(stuck) };
        let t0 = Instant::now();
        tap.stop();
        assert!(t0.elapsed() < STOP_JOIN + Duration::from_millis(500), "stop took {:?}", t0.elapsed());
        assert!(stop.load(Ordering::SeqCst));
        // A worker that ends promptly is joined.
        let quick = std::thread::spawn(|| ());
        let mut tap = TeamsTap { target: Target::Teams, stop: Arc::new(AtomicBool::new(false)), thread: Some(quick) };
        let t0 = Instant::now();
        tap.stop();
        assert!(t0.elapsed() < Duration::from_millis(500));
        assert!(tap.thread.is_none());
    }

    #[test]
    fn nothing_connects_once_stopped() {
        let stopped = AtomicBool::new(true);
        let seen = Seen { teams_running: true, teams: vec![1], own: None };
        let (tap, mode) = connect(Target::Teams, &seen, true, &stopped);
        assert!(tap.is_none());
        assert_eq!(mode, CaptureMode::MicOnly);
        assert!(connect(Target::AllSound, &seen, true, &stopped).0.is_none());
    }

    #[test]
    #[ignore = "live: creates a Core Audio tap"]
    fn live_tap_opened_after_stop_never_starts() {
        let stopped = AtomicBool::new(true);
        let own: Vec<AudioObjectID> = own_process_object().into_iter().collect();
        let err = open_tap(&own, true, &stopped).err().expect("refused");
        eprintln!("opening after stop: {err}");
        assert_eq!(err.to_string(), STOPPED);
        // And a healthy private aggregate reports itself alive (DeviceIsAlive reads 1).
        let tap = open_tap(&own, true, &RUN).expect("all-sound tap");
        let alive = get::<u32>(tap.agg, kAudioDevicePropertyDeviceIsAlive, None);
        eprintln!("healthy aggregate DeviceIsAlive = {alive:?}");
        assert_eq!(alive, Some(1));
        assert!(tap.device_alive());
    }

    #[test]
    #[ignore = "live: plays a tone; changes a private aggregate's rate; needs System Audio Recording permission"]
    fn live_aggregate_rate_change_is_resampled_at_the_new_rate() {
        let wav = std::env::temp_dir().join(format!("wren-tone-{}.wav", uuid::Uuid::new_v4()));
        tone_wav(&wav, 9.0);
        let mut play = std::process::Command::new("/usr/bin/afplay").arg(&wav).spawn().unwrap();
        let pid = play.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        let obj = loop {
            if let Some(p) = audio_processes().into_iter().find(|p| p.pid == pid) {
                break p.object;
            }
            assert!(Instant::now() < deadline, "afplay never registered with Core Audio");
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut conn = Conn::new(Some(open_tap(&[obj], false, &RUN).expect("process tap")), CaptureMode::TeamsOnly);
        let opened = conn.tap.as_ref().unwrap().rate;
        let other = if opened == 48_000 { 24_000 } else { 48_000 };
        let (mut raw, mut out) = (Vec::new(), Vec::new());
        let run = |conn: &mut Conn, raw: &mut Vec<f32>, out: &mut Vec<f32>, ms: u64| {
            let end = Instant::now() + Duration::from_millis(ms);
            while Instant::now() < end {
                std::thread::sleep(TICK);
                conn.pump(raw, out);
            }
        };
        // A freshly built binary's first tap can take a moment to start delivering: measure from then.
        let deadline = Instant::now() + Duration::from_secs(5);
        while conn.tap.as_ref().unwrap().callbacks() < 20 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        conn.pump(&mut raw, &mut out);
        out.clear();
        run(&mut conn, &mut raw, &mut out, 2_000);
        let before = out.split_off(0);
        // What a Bluetooth headset switching to hands-free looks like to the IO proc.
        set_nominal_rate(conn.tap.as_ref().unwrap().agg, other as f64);
        run(&mut conn, &mut raw, &mut out, 3_000);
        let t = conn.tap.as_ref().unwrap();
        let (now_rate, boundaries, implied, changed) = (t.rate, t.boundaries(), t.implied_rate(), t.format_changed());
        drop(conn);
        let _ = play.kill();
        let _ = play.wait();
        let _ = std::fs::remove_file(&wav);
        let after = &out[out.len().saturating_sub(32_000)..];
        let (f_before, f_after) = (frequency(&before[8_000..], 16_000.0), frequency(after, 16_000.0));
        eprintln!(
            "aggregate {opened} -> {other} Hz: resampling at {now_rate} Hz, {boundaries} boundaries, implied {implied:?}, \
             format changed {changed}; tone before {f_before:.0} Hz, after {f_after:.0} Hz, {} / {} samples",
            before.len(),
            out.len()
        );
        assert_eq!(now_rate, other, "resampler follows the new rate");
        assert!(out.len() >= 16_000 * 5 / 2, "no audio lost across the switch");
        assert!(!changed, "no reconnect needed");
        assert!((f_before - 1_000.0).abs() < 30.0);
        assert!((f_after - 1_000.0).abs() < 30.0, "tone keeps its pitch after the rate change");
    }

    fn p(object: u32, pid: i32, ppid: i32, bundle: &str) -> AudioProc {
        AudioProc { object, pid, ancestors: if ppid > 0 { vec![ppid] } else { vec![] }, bundle_id: bundle.into() }
    }

    #[test]
    fn teams_bundles_match_new_classic_and_helpers_only() {
        assert!(is_teams_bundle("com.microsoft.teams2"));
        assert!(is_teams_bundle("com.microsoft.teams2.helper"));
        assert!(is_teams_bundle("com.microsoft.teams2.helper.Renderer"));
        assert!(is_teams_bundle("com.microsoft.teams"));
        assert!(is_teams_bundle("COM.Microsoft.Teams2"));
        assert!(!is_teams_bundle("com.microsoft.teamsfoo"));
        assert!(!is_teams_bundle("com.microsoft.Word"));
        assert!(!is_teams_bundle(""));
    }

    #[test]
    fn teams_objects_include_helpers_and_children() {
        let procs = [
            p(80, 500, 1, "com.microsoft.teams2"),
            p(81, 510, 500, "com.microsoft.teams2.helper.Renderer"),
            p(82, 520, 500, ""), // a child without a bundle id
            p(83, 600, 1, "com.google.Chrome"),
            p(84, 700, 1, "uk.wren.app"),
        ];
        assert_eq!(teams_objects(&procs, &[500]), vec![80, 81, 82]);
        // Bundle ids alone are enough when the app list isn't available.
        assert_eq!(teams_objects(&procs, &[]), vec![80, 81]);
    }

    #[test]
    fn teams_objects_include_grandchildren_within_hop_limit() {
        let deep = |object: u32, pid: i32, ancestors: &[i32]| AudioProc { object, pid, ancestors: ancestors.to_vec(), bundle_id: String::new() };
        let procs = [
            p(80, 500, 1, "com.microsoft.teams2"),
            // Teams → WebView host (510) → renderer (520) → audio helper (530).
            deep(81, 520, &[510, 500, 1]),
            deep(82, 530, &[520, 510, 500]),
            // Four hops down: still Teams.
            deep(83, 540, &[530, 520, 510, 500]),
            // Unrelated tree.
            deep(84, 900, &[800, 700, 1]),
        ];
        assert_eq!(teams_objects(&procs, &[500]), vec![80, 81, 82, 83]);
    }

    #[test]
    fn teams_objects_empty_when_teams_closed() {
        assert!(teams_objects(&[p(83, 600, 1, "com.google.Chrome")], &[]).is_empty());
    }

    fn mixed(planes: &[(&[f32], usize)]) -> Vec<f32> {
        let ring = Ring::new(64);
        mix_to_mono(planes, &ring);
        let mut out = Vec::new();
        ring.drain_into(&mut out);
        out
    }

    #[test]
    fn mix_mono_passes_through() {
        assert_eq!(mixed(&[(&[0.1, 0.2, 0.3], 1)]), vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn mix_interleaved_stereo_averages() {
        assert_eq!(mixed(&[(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2)]), vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn mix_planar_buffers_averages_and_uses_shortest() {
        assert_eq!(mixed(&[(&[1.0, 1.0, 1.0], 1), (&[0.0, 0.0], 1)]), vec![0.5, 0.5]);
    }

    #[test]
    fn mix_ignores_empty_input() {
        assert!(mixed(&[]).is_empty());
        assert!(mixed(&[(&[], 2)]).is_empty());
        // An empty buffer next to a real one doesn't zero the frame count.
        assert_eq!(mixed(&[(&[], 1), (&[0.4, 0.2], 1)]), vec![0.4, 0.2]);
    }

    #[test]
    fn ring_keeps_order_across_wraparound() {
        let ring = Ring::new(8);
        let mut out = Vec::new();
        for round in 0..5 {
            ring.push_with(6, |i| (round * 10 + i) as f32);
            out.clear();
            ring.drain_into(&mut out);
            assert_eq!(out, (0..6).map(|i| (round * 10 + i) as f32).collect::<Vec<_>>());
        }
        assert_eq!(ring.dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ring_drops_what_doesnt_fit_instead_of_blocking() {
        let ring = Ring::new(8);
        ring.push_with(5, |i| i as f32);
        ring.push_with(5, |i| 10.0 + i as f32);
        let mut out = Vec::new();
        ring.drain_into(&mut out);
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0, 4.0, 10.0, 11.0, 12.0]);
        assert_eq!(ring.dropped.load(Ordering::Relaxed), 2);
        out.clear();
        ring.drain_into(&mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn ring_capacity_is_a_power_of_two_at_least_asked() {
        assert_eq!(Ring::new(96_000).buf.len(), 131_072);
        assert_eq!(Ring::new(0).buf.len(), 2);
    }

    #[test]
    fn ring_hands_samples_across_threads() {
        let ring = Arc::new(Ring::new(1024));
        let producer = {
            let ring = ring.clone();
            std::thread::spawn(move || {
                for chunk in 0..200 {
                    // Wait for room so nothing is dropped.
                    while ring.head.load(Ordering::Acquire).wrapping_sub(ring.tail.load(Ordering::Acquire)) > 1024 - 100 {
                        std::thread::yield_now();
                    }
                    ring.push_with(100, |i| (chunk * 100 + i) as f32);
                }
            })
        };
        let mut got = Vec::new();
        while got.len() < 20_000 {
            ring.drain_into(&mut got);
        }
        producer.join().unwrap();
        assert!(got.iter().enumerate().all(|(i, v)| *v == i as f32));
    }

    #[test]
    fn silence_alone_is_not_a_stall() {
        let long = STALL + Duration::from_millis(1);
        // Teams idle: its tap gets no callbacks at all.
        assert!(!is_stalled(long, true, false));
        // Teams is playing but nothing reaches us: dead.
        assert!(is_stalled(long, true, true));
        // The aggregate device went away (device change, sleep/wake).
        assert!(is_stalled(long, false, false));
        // Not silent for long enough yet.
        assert!(!is_stalled(STALL, false, true));
    }

    #[test]
    fn rate_change_needs_both_rates_known() {
        assert!(rate_changed(Some(48_000), Some(24_000)));
        assert!(!rate_changed(Some(48_000), Some(48_000)));
        assert!(!rate_changed(Some(48_000), None), "unreadable is not a change");
        assert!(!rate_changed(None, Some(24_000)));
    }

    fn check(target: Target, mode: CaptureMode, has_tap: bool) -> Check {
        Check {
            target,
            mode,
            has_tap,
            stalled: false,
            format_changed: false,
            rate_unknown: false,
            seen_changed: false,
            teams_running: true,
            teams_has_audio: true,
            upgrade_due: false,
            upgrade_busy: false,
        }
    }

    #[test]
    fn both_taps_failed_while_teams_runs_retries() {
        // Teams and all-sound taps both failed: MicOnly with no tap, nothing in `Seen` changes.
        assert_eq!(decide(&check(Target::Teams, CaptureMode::MicOnly, false)), Action::Reconnect);
        // Teams closed: stay mic-only until it appears (a `Seen` change).
        let closed = Check { teams_running: false, teams_has_audio: false, ..check(Target::Teams, CaptureMode::MicOnly, false) };
        assert_eq!(decide(&closed), Action::Keep);
        // Teams open but not using audio yet: nothing to tap; its audio appearing is a `Seen` change.
        let idle = Check { teams_has_audio: false, ..check(Target::Teams, CaptureMode::TeamsOnly, false) };
        assert_eq!(decide(&idle), Action::Keep);
        assert_eq!(decide(&Check { seen_changed: true, ..idle }), Action::Reconnect);
    }

    #[test]
    fn dead_or_reformatted_tap_reconnects() {
        let healthy = check(Target::Teams, CaptureMode::TeamsOnly, true);
        assert_eq!(decide(&healthy), Action::Keep);
        assert_eq!(decide(&Check { stalled: true, ..healthy }), Action::Reconnect);
        assert_eq!(decide(&Check { format_changed: true, ..healthy }), Action::Reconnect);
        let fallback = check(Target::AllSound, CaptureMode::AllSound, true);
        assert_eq!(decide(&Check { stalled: true, ..fallback }), Action::Reconnect);
        assert_eq!(decide(&Check { format_changed: true, ..fallback }), Action::Reconnect);
        assert_eq!(decide(&check(Target::AllSound, CaptureMode::MicOnly, false)), Action::Reconnect);
    }

    #[test]
    fn all_sound_fallback_retries_teams_every_30s() {
        let fallback = check(Target::Teams, CaptureMode::AllSound, true);
        assert_eq!(decide(&fallback), Action::Keep);
        assert_eq!(decide(&Check { upgrade_due: true, ..fallback }), Action::TryTeams);
        // A retry already opening (or open and waiting to deliver): don't start another.
        assert_eq!(decide(&Check { upgrade_due: true, upgrade_busy: true, ..fallback }), Action::Keep);
        // Nothing of Teams' to tap: no point trying.
        assert_eq!(decide(&Check { upgrade_due: true, teams_has_audio: false, ..fallback }), Action::Keep);
        // A Teams-only capture never "upgrades".
        let teams = check(Target::Teams, CaptureMode::TeamsOnly, true);
        assert_eq!(decide(&Check { upgrade_due: true, ..teams }), Action::Keep);
    }

    #[test]
    fn seen_changes_that_matter_per_mode() {
        let a = Seen { teams_running: true, teams: vec![80], own: Some(5) };
        let own_moved = Seen { own: Some(6), ..a.clone() };
        assert!(!own_moved.changed_from(&a, Target::Teams, CaptureMode::TeamsOnly));
        assert!(own_moved.changed_from(&a, Target::Teams, CaptureMode::AllSound));
        assert!(own_moved.changed_from(&a, Target::AllSound, CaptureMode::AllSound));
        let helper = Seen { teams: vec![80, 81], ..a.clone() };
        assert!(helper.changed_from(&a, Target::Teams, CaptureMode::TeamsOnly));
        assert!(!helper.changed_from(&a, Target::AllSound, CaptureMode::AllSound));
    }

    // ---- live (ignored) ----

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
    #[ignore = "live: plays a tone through the speakers; needs System Audio Recording permission"]
    fn live_all_sound_tap_hears_tone() {
        eprintln!("system audio permission: {}", system_audio_permission());
        let wav = std::env::temp_dir().join(format!("wren-tone-{}.wav", uuid::Uuid::new_v4()));
        tone_wav(&wav, 2.5);
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsTap::new(Target::AllSound);
        assert_eq!(src.start(tx, mtx).unwrap(), CaptureMode::AllSound);
        // `afplay` is its own process (own Core Audio process object), so the global tap that
        // excludes only this test process hears it.
        let mut play = std::process::Command::new("/usr/bin/afplay").arg(&wav).spawn().unwrap();
        let samples = collect(&rx, 5_000);
        src.stop();
        let _ = play.wait();
        let _ = std::fs::remove_file(&wav);
        let rms = (samples.iter().map(|x| x * x).sum::<f32>() / samples.len().max(1) as f32).sqrt();
        eprintln!("captured {} samples, rms {rms:.4}", samples.len());
        assert!(samples.len() as f32 >= 16_000.0 * 4.5, "wall-clock aligned stream");
        assert!(rms > 0.01, "heard the tone");
    }

    #[test]
    #[ignore = "live: needs Teams running"]
    fn live_teams_tap_attaches_when_teams_running() {
        let seen = look();
        eprintln!("teams running {}, {} Teams audio objects", seen.teams_running, seen.teams.len());
        if !seen.teams_running {
            eprintln!("Teams isn't running; skipped");
            return;
        }
        if !seen.teams.is_empty() {
            // Liveness relies on the tap calling back even while Teams is silent.
            let tap = open_tap(&seen.teams, false, &RUN).expect("Teams tap");
            let first = tap.callbacks();
            std::thread::sleep(Duration::from_millis(4_000));
            let calls = tap.callbacks() - first;
            eprintln!(
                "Teams tap: rate {} Hz (tap {:?}, device {:?}), {calls} IO callbacks in 4 s, format changed {}",
                tap.rate,
                tap.tap_rate(),
                tap.device_rate(),
                tap.format_changed()
            );
            let playing = tap.sources_playing();
            eprintln!("Teams playing: {playing}, device alive: {}", tap.device_alive());
            if !playing {
                // An idle Teams delivers nothing; that must not count as a stall.
                assert!(!is_stalled(STALL * 2, tap.device_alive(), playing));
            } else {
                assert!(calls > 100, "a playing Teams calls back");
            }
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsTap::new(Target::Teams);
        let mode = src.start(tx, mtx).unwrap();
        // Long enough for a recheck and a stall window: a healthy tap must not be reconnected.
        let samples = collect(&rx, 7_000);
        src.stop();
        let changes: Vec<CaptureMode> = mrx.try_iter().collect();
        eprintln!("mode {mode:?}, {} samples, mode changes {changes:?}", samples.len());
        assert_eq!(mode, CaptureMode::TeamsOnly);
        assert!(changes.is_empty());
        assert!(samples.len() >= 16_000 * 6, "silence keeps the stream aligned");
    }

    #[test]
    #[ignore = "live: plays a tone; needs System Audio Recording permission"]
    fn live_process_tap_hears_that_process() {
        // The Teams path, exercised on a process that is always available: tap only `afplay`.
        let wav = std::env::temp_dir().join(format!("wren-tone-{}.wav", uuid::Uuid::new_v4()));
        tone_wav(&wav, 4.0);
        let mut play = std::process::Command::new("/usr/bin/afplay").arg(&wav).spawn().unwrap();
        let pid = play.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        let obj = loop {
            if let Some(p) = audio_processes().into_iter().find(|p| p.pid == pid) {
                break p.object;
            }
            assert!(Instant::now() < deadline, "afplay never registered with Core Audio");
            std::thread::sleep(Duration::from_millis(50));
        };
        let tap = open_tap(&[obj], false, &RUN).expect("process tap");
        let rate = tap.rate;
        let end = Instant::now() + Duration::from_millis(1_500);
        let mut raw = Vec::new();
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
            tap.drain_into(&mut raw);
        }
        let callbacks = tap.callbacks();
        let (boundaries, implied) = (tap.boundaries(), tap.implied_rate());
        drop(tap);
        eprintln!("{callbacks} IO callbacks in 1.5 s, {boundaries} boundaries, implied rate {implied:?}");
        assert!(boundaries <= 1, "a steady tap doesn't flag format boundaries");
        let _ = play.kill();
        let _ = play.wait();
        let _ = std::fs::remove_file(&wav);
        let rms = (raw.iter().map(|x| x * x).sum::<f32>() / raw.len().max(1) as f32).sqrt();
        eprintln!("tap rate {rate} Hz, {} raw samples, rms {rms:.4}", raw.len());
        assert!(rms > 0.01, "heard afplay through its own process tap");
    }

    fn set_nominal_rate(device: AudioObjectID, rate: f64) {
        let mut addr = address(objc2_core_audio::kAudioDevicePropertyNominalSampleRate);
        let st = unsafe {
            objc2_core_audio::AudioObjectSetPropertyData(
                device,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                std::mem::size_of::<f64>() as u32,
                NonNull::from(&rate).cast(),
            )
        };
        assert_eq!(st, 0, "set nominal sample rate");
    }

    /// Dominant frequency from zero crossings.
    fn frequency(samples: &[f32], rate: f32) -> f32 {
        let crossings = samples.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
        crossings as f32 / 2.0 / (samples.len() as f32 / rate)
    }

    #[test]
    #[ignore = "live: switches the default output device's sample rate for a few seconds (restored); plays a tone"]
    fn live_output_rate_change_keeps_pitch() {
        let device: AudioObjectID =
            get(kAudioObjectSystemObject as u32, objc2_core_audio::kAudioHardwarePropertyDefaultOutputDevice, None).unwrap();
        let original = device_rate(device).expect("output rate");
        let other = if original == 48_000 { 44_100 } else { 48_000 };
        let wav = std::env::temp_dir().join(format!("wren-tone-{}.wav", uuid::Uuid::new_v4()));
        tone_wav(&wav, 9.0);
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsTap::new(Target::AllSound);
        assert_eq!(src.start(tx, mtx).unwrap(), CaptureMode::AllSound);
        let mut play = std::process::Command::new("/usr/bin/afplay").arg(&wav).spawn().unwrap();
        // A second, bare tap to watch what Core Audio reports.
        let probe = open_tap(&own_process_object().into_iter().collect::<Vec<_>>(), true, &RUN).expect("probe tap");
        let before = collect(&rx, 2_500);
        set_nominal_rate(device, other as f64);
        let switched = collect(&rx, 4_000);
        eprintln!(
            "probe tap after switch: tap {:?} Hz, device {:?} Hz (opened at {} Hz), format changed {}, {} boundaries, implied {:?} Hz, alive {}",
            tap_rate(probe.tap),
            device_rate(probe.agg),
            probe.rate,
            probe.format_changed(),
            probe.boundaries(),
            probe.implied_rate(),
            probe.device_alive()
        );
        drop(probe);
        set_nominal_rate(device, original as f64);
        let until = Instant::now() + Duration::from_secs(3);
        while device_rate(device) != Some(original) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = play.kill();
        let _ = play.wait();
        src.stop();
        let _ = std::fs::remove_file(&wav);
        // Skip the first second after the switch (reconnect window), measure the rest.
        let after = &switched[16_000..];
        let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
        let before = &before[16_000..];
        let (f_before, f_after) = (frequency(before, 16_000.0), frequency(after, 16_000.0));
        eprintln!(
            "output {original} -> {other} Hz: before {f_before:.0} Hz rms {:.3}, after {f_after:.0} Hz rms {:.3}, modes {:?}",
            rms(before),
            rms(after),
            mrx.try_iter().collect::<Vec<_>>()
        );
        assert_eq!(device_rate(device), Some(original), "output rate restored");
        assert!((f_before - 1_000.0).abs() < 30.0);
        assert!(rms(after) > 0.01, "still capturing after the rate change");
        assert!((f_after - 1_000.0).abs() < 30.0, "tone keeps its pitch after the rate change");
    }

    #[test]
    #[ignore = "live: plays a tone; needs System Audio Recording permission"]
    fn live_tap_that_stops_calling_back_while_playing_is_stalled() {
        let wav = std::env::temp_dir().join(format!("wren-tone-{}.wav", uuid::Uuid::new_v4()));
        tone_wav(&wav, 9.0);
        let mut play = std::process::Command::new("/usr/bin/afplay").arg(&wav).spawn().unwrap();
        let pid = play.id() as i32;
        let deadline = Instant::now() + Duration::from_secs(3);
        let obj = loop {
            if let Some(p) = audio_processes().into_iter().find(|p| p.pid == pid) {
                break p.object;
            }
            assert!(Instant::now() < deadline, "afplay never registered with Core Audio");
            std::thread::sleep(Duration::from_millis(50));
        };
        let mut conn = Conn::new(Some(open_tap(&[obj], false, &RUN).expect("process tap")), CaptureMode::TeamsOnly);
        let (mut raw, mut out) = (Vec::new(), Vec::new());
        std::thread::sleep(Duration::from_millis(1_000));
        conn.pump(&mut raw, &mut out);
        let healthy = conn.stalled();
        // What a lost device looks like from here: the IO proc stops while afplay keeps playing.
        let t = conn.tap.as_ref().unwrap();
        assert_eq!(unsafe { AudioDeviceStop(t.agg, t.proc_id) }, 0);
        // The worker pumps every tick, so it sees the last callback promptly.
        conn.pump(&mut raw, &mut out);
        std::thread::sleep(STALL + Duration::from_millis(1_200));
        conn.pump(&mut raw, &mut out);
        let playing = conn.tap.as_ref().unwrap().sources_playing();
        let stalled = conn.stalled();
        drop(conn);
        let _ = play.kill();
        let _ = play.wait();
        let _ = std::fs::remove_file(&wav);
        eprintln!("healthy tap stalled {healthy}; after stopping: afplay playing {playing}, stalled {stalled}");
        assert!(!healthy);
        assert!(playing && stalled);
    }

    #[test]
    #[ignore = "live: lists audio processes"]
    fn live_no_teams_means_mic_only() {
        if !running_teams_pids().is_empty() {
            eprintln!("Teams is running; skipped");
            return;
        }
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsTap::new(Target::Teams);
        let mode = src.start(tx, mtx).unwrap();
        let samples = collect(&rx, 1_500);
        src.stop();
        eprintln!("mode {mode:?}, {} samples, {} audio processes", samples.len(), audio_processes().len());
        assert_eq!(mode, CaptureMode::MicOnly);
        assert!(samples.len() >= 16_000, "silence keeps the stream aligned");
        assert!(samples.iter().all(|s| *s == 0.0));
    }
}
