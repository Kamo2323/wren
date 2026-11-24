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
//! back for [`STALL`], and when the output device's sample rate changes under it (AirPods 48 kHz ↔
//! hands-free 16/24 kHz, sleep/wake), so the resampler always runs at the real rate. On the all-sound
//! fallback it retries Teams-only every [`UPGRADE_RETRY`] and switches only if that opens. Mode
//! changes are reported on `mode_tx`. Wall-clock alignment: when too little arrives, zeros are emitted so
//! `stream_pos_ms` tracks real time. Audio is never logged.

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
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat};
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
/// Retries that keep failing log a warning at most this often.
const QUIET_RETRY_LOGS: Duration = Duration::from_secs(60);
/// Seconds of audio buffered between the Core Audio IO thread and the worker (it drains every tick).
const RING_SECS: usize = 2;

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
        }
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

    /// Consumer: move everything available onto the end of `out`.
    fn drain_into(&self, out: &mut Vec<f32>) {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let n = head.wrapping_sub(tail).min(self.buf.len());
        out.reserve(n);
        for i in 0..n {
            if let Some(slot) = self.buf.get(tail.wrapping_add(i) & self.mask) {
                out.push(f32::from_bits(slot.load(Ordering::Relaxed)));
            }
        }
        self.tail.store(tail.wrapping_add(n), Ordering::Release);
    }
}

/// Mix Core Audio buffers (each interleaved with its own channel count) down to mono into `ring`,
/// without allocating. Buffers without data are skipped; the shortest one sets the frame count.
fn mix_to_mono<P: Plane>(planes: &[P], ring: &Ring) {
    let live = || planes.iter().filter(|p| !p.samples().is_empty());
    let total_ch: usize = live().map(|p| p.channels()).sum();
    let frames = live().map(|p| p.samples().len() / p.channels()).min().unwrap_or(0);
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

/// Shared between the worker and the Core Audio IO thread.
struct Ctx {
    ring: Ring,
    /// IO callbacks so far (liveness: a tap whose device stopped calling back is reopened).
    callbacks: AtomicU64,
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
    _input_time: NonNull<AudioTimeStamp>,
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

/// True when a rate known at open time is now known to be different (unreadable is not a change).
fn rate_changed(opened: Option<u32>, now: Option<u32>) -> bool {
    matches!((opened, now), (Some(a), Some(b)) if a != b)
}

/// A running process tap + private aggregate device + IO proc. Torn down on drop.
struct Tap {
    tap: AudioObjectID,
    agg: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    ctx: *mut Ctx,
    /// The rate audio arrives at (the device's, else the tap's).
    rate: u32,
    opened_tap_rate: Option<u32>,
    opened_device_rate: Option<u32>,
    /// The process objects tapped (or, with `exclude`, the ones left out).
    objects: Vec<AudioObjectID>,
    exclude: bool,
}

impl Tap {
    fn device_alive(&self) -> bool {
        get::<u32>(self.agg, kAudioDevicePropertyDeviceIsAlive, None) != Some(0)
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

    fn drain_into(&self, out: &mut Vec<f32>) {
        if let Some(c) = self.ctx() {
            c.ring.drain_into(out);
        }
    }

    fn callbacks(&self) -> u64 {
        self.ctx().map_or(0, |c| c.callbacks.load(Ordering::Relaxed))
    }

    /// The output device changed under the tap (AirPods 48 kHz ↔ hands-free 16/24 kHz, sleep/wake):
    /// the resampler would now run at the wrong rate, so the tap must be rebuilt.
    fn format_changed(&self) -> bool {
        rate_changed(self.opened_tap_rate, tap_rate(self.tap)) || rate_changed(self.opened_device_rate, device_rate(self.agg))
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

/// Tap `objects` (or, with `exclude`, every process except them) and start receiving audio.
fn open_tap(objects: &[AudioObjectID], exclude: bool) -> anyhow::Result<Tap> {
    let mut tap = Tap {
        tap: kAudioObjectUnknown,
        agg: kAudioObjectUnknown,
        proc_id: None,
        ctx: std::ptr::null_mut(),
        rate: 0,
        opened_tap_rate: None,
        opened_device_rate: None,
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
    tap.opened_tap_rate = tap_rate(tap.tap);

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

    tap.opened_device_rate = device_rate(tap.agg);
    tap.rate = tap
        .opened_device_rate
        .or(tap.opened_tap_rate)
        .ok_or_else(|| anyhow::anyhow!("tap has no usable sample rate"))?;
    tap.ctx = Box::into_raw(Box::new(Ctx {
        ring: Ring::new(tap.rate as usize * RING_SECS),
        callbacks: AtomicU64::new(0),
    }));
    // `extern "C"` and `extern "C-unwind"` share the calling convention; only unwinding differs, and
    // `io_proc` never unwinds (a panic aborts at its boundary).
    let proc: AudioDeviceIOProc = Some(unsafe { std::mem::transmute::<IoProc, IoProcUnwind>(io_proc as IoProc) });
    status("AudioDeviceCreateIOProcID", unsafe {
        AudioDeviceCreateIOProcID(tap.agg, proc, tap.ctx.cast(), NonNull::from(&mut tap.proc_id))
    })?;
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

fn all_sound(seen: &Seen, loud: bool) -> (Option<Tap>, CaptureMode) {
    let exclude: Vec<AudioObjectID> = seen.own.into_iter().collect();
    match open_tap(&exclude, true) {
        Ok(t) => (Some(t), CaptureMode::AllSound),
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

/// Open the capture for `target`. `loud` logs failures as warnings (retries log quietly).
fn connect(target: Target, seen: &Seen, loud: bool) -> (Option<Tap>, CaptureMode) {
    match target {
        Target::AllSound => all_sound(seen, loud),
        Target::Teams if !seen.teams_running => (None, CaptureMode::MicOnly),
        // Teams is running but hasn't opened audio yet: nothing to tap until it does (rechecked).
        Target::Teams if seen.teams.is_empty() => (None, CaptureMode::TeamsOnly),
        Target::Teams => match open_tap(&seen.teams, false) {
            Ok(t) => (Some(t), CaptureMode::TeamsOnly),
            Err(e) => {
                if loud {
                    tracing::warn!(error = %e, "Teams tap failed; capturing all computer sound");
                } else {
                    tracing::debug!(error = %e, "Teams tap failed again");
                }
                all_sound(seen, loud)
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
    /// The tap's / device's sample rate moved since the tap was opened.
    format_changed: bool,
    /// Teams appeared, quit, restarted or changed its audio processes (see [`Seen::changed_from`]).
    seen_changed: bool,
    teams_running: bool,
    /// Teams has audio process objects to tap.
    teams_has_audio: bool,
    /// [`UPGRADE_RETRY`] passed since the last Teams-only attempt.
    upgrade_due: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Keep,
    /// Tear the capture down and connect again from scratch.
    Reconnect,
    /// On the all-sound fallback: open a Teams-only tap and switch only if it opens.
    TryTeams,
}

fn decide(c: &Check) -> Action {
    if c.seen_changed || (c.has_tap && (c.stalled || c.format_changed)) {
        return Action::Reconnect;
    }
    match c.target {
        Target::AllSound if !c.has_tap => Action::Reconnect,
        Target::AllSound => Action::Keep,
        // Both taps failed (or the tap was lost) while Teams has audio: try again.
        Target::Teams if !c.has_tap && c.teams_running && c.teams_has_audio => Action::Reconnect,
        Target::Teams if c.mode == CaptureMode::AllSound && c.has_tap && c.teams_has_audio && c.upgrade_due => {
            Action::TryTeams
        }
        Target::Teams => Action::Keep,
    }
}

/// The current capture: tap (if any), its resampler and liveness bookkeeping.
struct Conn {
    tap: Option<Tap>,
    mode: CaptureMode,
    resampler: Option<Resampler16k>,
    callbacks: u64,
    last_io: Instant,
}

impl Conn {
    fn new(tap: Option<Tap>, mode: CaptureMode) -> Self {
        let resampler = tap.as_ref().and_then(|t| match Resampler16k::new(t.rate) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(error = %e, rate = t.rate, "cannot resample Teams audio");
                None
            }
        });
        Self { tap, mode, resampler, callbacks: 0, last_io: Instant::now() }
    }

    /// Move what the IO thread delivered through the resampler into `out`.
    fn pump(&mut self, raw: &mut Vec<f32>, out: &mut Vec<f32>) {
        let Some(t) = &self.tap else { return };
        let n = t.callbacks();
        if n != self.callbacks {
            self.callbacks = n;
            self.last_io = Instant::now();
        }
        raw.clear();
        t.drain_into(raw);
        if let Some(r) = self.resampler.as_mut() {
            let _ = r.push(raw, out);
        }
    }

    /// Pump and flush the resampler's tail before this capture is replaced.
    fn finish(&mut self, raw: &mut Vec<f32>, out: &mut Vec<f32>) {
        self.pump(raw, out);
        if let Some(r) = self.resampler.as_mut() {
            let _ = r.finish(out);
        }
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
            let (tap, mode) = connect(target, &seen, true);
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
            let send = |samples: Vec<f32>, emitted: &mut u64| {
                let pos = *emitted * 1000 / RATE;
                *emitted += samples.len() as u64;
                tx.send(AudioBlock { samples, stream_pos_ms: pos }).is_ok()
            };
            'run: while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                conn.pump(&mut raw, &mut out);
                if !out.is_empty() && !send(std::mem::take(&mut out), &mut emitted) {
                    break 'run;
                }
                // Keep up with real time when nothing (or too little) arrives.
                let real = started.elapsed().as_millis() as u64;
                let have = emitted * 1000 / RATE;
                if real > have + PAD_AFTER_MS {
                    let missing = ((real - have) * RATE / 1000) as usize;
                    if !send(vec![0.0; missing], &mut emitted) {
                        break 'run;
                    }
                }

                let (format_changed, stalled) = if last_health.elapsed() >= HEALTH_CHECK {
                    last_health = Instant::now();
                    (conn.tap.as_ref().is_some_and(Tap::format_changed), conn.stalled())
                } else {
                    (false, false)
                };
                if !(format_changed || stalled || last_check.elapsed() >= RECHECK) {
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
                    seen_changed: now.changed_from(&seen, target, conn.mode),
                    teams_running: now.teams_running,
                    teams_has_audio: !now.teams.is_empty(),
                    upgrade_due: last_upgrade.elapsed() >= UPGRADE_RETRY,
                };
                let next = match decide(&check) {
                    Action::Keep => None,
                    Action::Reconnect => {
                        // Retries that keep happening (both taps failing, a tap that keeps dying)
                        // log at most once a minute; Teams starting or quitting always logs.
                        let loud = check.seen_changed || last_loud.elapsed() >= QUIET_RETRY_LOGS;
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
                        drop(conn.tap.take());
                        let (t, m) = connect(target, &now, loud);
                        if m == CaptureMode::AllSound && conn.mode != CaptureMode::AllSound {
                            last_upgrade = Instant::now();
                        }
                        Some(Conn::new(t, m))
                    }
                    Action::TryTeams => {
                        last_upgrade = Instant::now();
                        match open_tap(&now.teams, false) {
                            Ok(t) => {
                                tracing::info!("Teams tap opened; leaving the all-sound fallback");
                                conn.finish(&mut raw, &mut out);
                                Some(Conn::new(Some(t), CaptureMode::TeamsOnly))
                            }
                            Err(e) => {
                                tracing::debug!(error = %e, "Teams tap still failing; staying on all sound");
                                None
                            }
                        }
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
                if !out.is_empty() && !send(std::mem::take(&mut out), &mut emitted) {
                    break 'run;
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
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
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
            seen_changed: false,
            teams_running: true,
            teams_has_audio: true,
            upgrade_due: false,
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
            let tap = open_tap(&seen.teams, false).expect("Teams tap");
            let first = tap.callbacks();
            std::thread::sleep(Duration::from_millis(4_000));
            let calls = tap.callbacks() - first;
            eprintln!(
                "Teams tap: rate {} Hz (tap {:?}, device {:?}), {calls} IO callbacks in 4 s, format changed {}",
                tap.rate,
                tap.opened_tap_rate,
                tap.opened_device_rate,
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
        let tap = open_tap(&[obj], false).expect("process tap");
        let rate = tap.rate;
        let end = Instant::now() + Duration::from_millis(1_500);
        let mut raw = Vec::new();
        while Instant::now() < end {
            std::thread::sleep(Duration::from_millis(50));
            tap.drain_into(&mut raw);
        }
        let callbacks = tap.callbacks();
        drop(tap);
        eprintln!("{callbacks} IO callbacks in 1.5 s");
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
        let probe = open_tap(&own_process_object().into_iter().collect::<Vec<_>>(), true).expect("probe tap");
        let before = collect(&rx, 2_500);
        set_nominal_rate(device, other as f64);
        let switched = collect(&rx, 4_000);
        eprintln!(
            "probe tap after switch: tap {:?} Hz, device {:?} Hz (opened at {:?} / {:?}), format changed {}",
            tap_rate(probe.tap),
            device_rate(probe.agg),
            probe.opened_tap_rate,
            probe.opened_device_rate,
            probe.format_changed()
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
        let mut conn = Conn::new(Some(open_tap(&[obj], false).expect("process tap")), CaptureMode::TeamsOnly);
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
