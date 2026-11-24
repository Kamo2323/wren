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
//! Audio arrives at the tap's rate (usually 48 kHz) as f32, is mixed to mono and resampled to 16 kHz
//! with [`Resampler16k`]. Wall-clock alignment: when too little arrives, zeros are emitted so
//! `stream_pos_ms` tracks real time. Audio is never logged.

use super::{AudioBlock, AudioSource};
use crate::audio::Resampler16k;
use crate::meetings::CaptureMode;
use crossbeam_channel::{Receiver, Sender};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, CATapMuteBehavior, kAudioAggregateDeviceIsPrivateKey,
    kAudioAggregateDeviceIsStackedKey, kAudioAggregateDeviceNameKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioHardwarePropertyProcessObjectList,
    kAudioHardwarePropertyTranslatePIDToProcessObject, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown, kAudioProcessPropertyBundleID,
    kAudioProcessPropertyPID, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, kAudioFormatFlagIsFloat};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};
use std::ffi::{CStr, c_void};
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const RATE: u64 = 16_000;
/// How often the loop wakes when nothing arrives.
const TICK: Duration = Duration::from_millis(100);
/// Pad with silence only when this far behind real time (the tap delivers every ~10 ms).
const PAD_AFTER_MS: u64 = 300;
/// How often to look for Teams appearing, restarting or quitting.
const RECHECK: Duration = Duration::from_secs(5);
/// Raw chunks buffered between the Core Audio IO thread and the worker (~2.5 s at 10 ms chunks).
const RAW_QUEUE: usize = 256;

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
    pub ppid: i32,
    pub bundle_id: String,
}

/// New Teams (`com.microsoft.teams2`), its helpers (`com.microsoft.teams2.*`) and classic Teams.
pub fn is_teams_bundle(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    ["com.microsoft.teams2", "com.microsoft.teams"]
        .iter()
        .any(|b| id == *b || id.strip_prefix(b).is_some_and(|rest| rest.starts_with('.')))
}

/// The process objects to tap for Teams: anything with a Teams bundle id, plus processes started by a
/// running Teams app (Chromium-style helpers that may carry their own bundle id). Sorted, unique.
pub fn teams_objects(procs: &[AudioProc], teams_pids: &[i32]) -> Vec<AudioObjectID> {
    let mut out: Vec<AudioObjectID> = procs
        .iter()
        .filter(|p| is_teams_bundle(&p.bundle_id) || teams_pids.contains(&p.pid) || teams_pids.contains(&p.ppid))
        .map(|p| p.object)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// Mix Core Audio buffers (each interleaved with its own channel count) down to mono, appending to `out`.
fn mix_to_mono(buffers: &[(&[f32], usize)], out: &mut Vec<f32>) {
    let total_ch: usize = buffers.iter().map(|(_, c)| (*c).max(1)).sum();
    let frames = buffers.iter().map(|(d, c)| d.len() / (*c).max(1)).min().unwrap_or(0);
    if total_ch == 0 || frames == 0 {
        return;
    }
    if buffers.len() == 1 && buffers[0].1 <= 1 {
        out.extend_from_slice(&buffers[0].0[..frames]);
        return;
    }
    let scale = 1.0 / total_ch as f32;
    out.reserve(frames);
    for f in 0..frames {
        let mut sum = 0.0;
        for (data, ch) in buffers {
            let ch = (*ch).max(1);
            sum += data[f * ch..f * ch + ch].iter().sum::<f32>();
        }
        out.push(sum * scale);
    }
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

fn parent_pid(pid: i32) -> i32 {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size) };
    if n == size { info.pbi_ppid as i32 } else { 0 }
}

/// Every process the audio system knows about.
pub fn audio_processes() -> Vec<AudioProc> {
    get_u32_list(kAudioObjectSystemObject as u32, kAudioHardwarePropertyProcessObjectList)
        .into_iter()
        .map(|object| {
            let pid = get::<i32>(object, kAudioProcessPropertyPID, None).unwrap_or(-1);
            AudioProc { object, pid, ppid: if pid > 0 { parent_pid(pid) } else { 0 }, bundle_id: get_string(object, kAudioProcessPropertyBundleID).unwrap_or_default() }
        })
        .collect()
}

/// Pids of running Teams apps (main app bundles).
pub fn running_teams_pids() -> Vec<i32> {
    objc2::rc::autoreleasepool(|_| {
        let apps = objc2_app_kit::NSWorkspace::sharedWorkspace().runningApplications();
        apps.iter()
            .filter(|a| a.bundleIdentifier().is_some_and(|b| is_teams_bundle(&b.to_string())))
            .map(|a| a.processIdentifier())
            .filter(|p| *p > 0)
            .collect()
    })
}

/// Wren's own process object, if Wren has registered with the audio system.
fn own_process_object() -> Option<AudioObjectID> {
    let pid = std::process::id() as i32;
    get::<AudioObjectID>(kAudioObjectSystemObject as u32, kAudioHardwarePropertyTranslatePIDToProcessObject, Some(&pid.to_ne_bytes()))
        .filter(|o| *o != kAudioObjectUnknown)
}

// ---- the tap ----

struct Ctx {
    tx: Sender<Vec<f32>>,
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let ctx = unsafe { &*(client as *const Ctx) };
    let list = input.as_ptr();
    let n = unsafe { (*list).mNumberBuffers } as usize;
    let bufs: &[AudioBuffer] = unsafe { std::slice::from_raw_parts((*list).mBuffers.as_ptr(), n) };
    let views: Vec<(&[f32], usize)> = bufs
        .iter()
        .filter(|b| !b.mData.is_null())
        .map(|b| {
            let len = b.mDataByteSize as usize / 4;
            (unsafe { std::slice::from_raw_parts(b.mData as *const f32, len) }, b.mNumberChannels as usize)
        })
        .collect();
    let mut mono = Vec::new();
    mix_to_mono(&views, &mut mono);
    if !mono.is_empty() {
        let _ = ctx.tx.try_send(mono);
    }
    0
}

/// A running process tap + private aggregate device + IO proc. Torn down on drop.
struct Tap {
    tap: AudioObjectID,
    agg: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    ctx: *mut Ctx,
    rate: u32,
    rx: Receiver<Vec<f32>>,
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            if self.proc_id.is_some() {
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
    let (tx, rx) = crossbeam_channel::bounded(RAW_QUEUE);
    let mut tap = Tap { tap: kAudioObjectUnknown, agg: kAudioObjectUnknown, proc_id: None, ctx: std::ptr::null_mut(), rate: 0, rx };

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
    tap.rate = format.mSampleRate.round() as u32;

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

    tap.ctx = Box::into_raw(Box::new(Ctx { tx }));
    status("AudioDeviceCreateIOProcID", unsafe {
        AudioDeviceCreateIOProcID(tap.agg, Some(io_proc), tap.ctx.cast(), NonNull::from(&mut tap.proc_id))
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

fn all_sound(seen: &Seen) -> (Option<Tap>, CaptureMode) {
    let exclude: Vec<AudioObjectID> = seen.own.into_iter().collect();
    match open_tap(&exclude, true) {
        Ok(t) => (Some(t), CaptureMode::AllSound),
        Err(e) => {
            tracing::warn!(error = %e, "all-sound tap failed; recording the mic only");
            (None, CaptureMode::MicOnly)
        }
    }
}

/// Open the capture for `target`.
fn connect(target: Target, seen: &Seen) -> (Option<Tap>, CaptureMode) {
    match target {
        Target::AllSound => all_sound(seen),
        Target::Teams if !seen.teams_running => (None, CaptureMode::MicOnly),
        // Teams is running but hasn't opened audio yet: nothing to tap until it does (rechecked).
        Target::Teams if seen.teams.is_empty() => (None, CaptureMode::TeamsOnly),
        Target::Teams => match open_tap(&seen.teams, false) {
            Ok(t) => (Some(t), CaptureMode::TeamsOnly),
            Err(e) => {
                tracing::warn!(error = %e, "Teams tap failed; capturing all computer sound");
                all_sound(seen)
            }
        },
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
            let (mut tap, mut mode) = connect(target, &seen);
            let mut resampler = tap.as_ref().and_then(|t| Resampler16k::new(t.rate).ok());
            {
                let late = late_thread.lock();
                if *late {
                    let _ = mode_tx.send(mode);
                } else {
                    let _ = first_tx.send(mode);
                }
            }
            let mut emitted: u64 = 0; // 16 kHz samples
            let mut out: Vec<f32> = Vec::new();
            let mut last_check = Instant::now();
            let send = |samples: Vec<f32>, emitted: &mut u64| {
                let pos = *emitted * 1000 / RATE;
                *emitted += samples.len() as u64;
                tx.send(AudioBlock { samples, stream_pos_ms: pos }).is_ok()
            };
            'run: while !stop.load(Ordering::Relaxed) {
                match &tap {
                    Some(t) => {
                        if let Ok(first) = t.rx.recv_timeout(TICK) {
                            for chunk in std::iter::once(first).chain(t.rx.try_iter()) {
                                if let Some(r) = resampler.as_mut() {
                                    let _ = r.push(&chunk, &mut out);
                                }
                            }
                        }
                    }
                    None => std::thread::sleep(TICK),
                }
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
                if last_check.elapsed() >= RECHECK {
                    last_check = Instant::now();
                    let now = look();
                    let changed = match target {
                        Target::Teams => {
                            now.teams_running != seen.teams_running
                                || now.teams != seen.teams
                                || (mode == CaptureMode::AllSound && now.own != seen.own)
                        }
                        Target::AllSound => now.own != seen.own,
                    };
                    if changed {
                        if let Some(r) = resampler.as_mut() {
                            let _ = r.finish(&mut out);
                        }
                        drop(tap.take());
                        seen = now;
                        let (t, m) = connect(target, &seen);
                        resampler = t.as_ref().and_then(|t| Resampler16k::new(t.rate).ok());
                        tap = t;
                        if m != mode {
                            mode = m;
                            let _ = mode_tx.send(m);
                        }
                    }
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
        AudioProc { object, pid, ppid, bundle_id: bundle.into() }
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
    fn teams_objects_empty_when_teams_closed() {
        assert!(teams_objects(&[p(83, 600, 1, "com.google.Chrome")], &[]).is_empty());
    }

    #[test]
    fn mix_mono_passes_through() {
        let mut out = Vec::new();
        mix_to_mono(&[(&[0.1, 0.2, 0.3], 1)], &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn mix_interleaved_stereo_averages() {
        let mut out = Vec::new();
        mix_to_mono(&[(&[1.0, 0.0, 0.5, 0.5, -1.0, 1.0], 2)], &mut out);
        assert_eq!(out, vec![0.5, 0.5, 0.0]);
    }

    #[test]
    fn mix_planar_buffers_averages_and_uses_shortest() {
        let mut out = Vec::new();
        mix_to_mono(&[(&[1.0, 1.0, 1.0], 1), (&[0.0, 0.0], 1)], &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn mix_ignores_empty_input() {
        let mut out = Vec::new();
        mix_to_mono(&[], &mut out);
        mix_to_mono(&[(&[], 2)], &mut out);
        assert!(out.is_empty());
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
        let (tx, rx) = crossbeam_channel::unbounded();
        let (mtx, _mrx) = crossbeam_channel::unbounded();
        let mut src = TeamsTap::new(Target::Teams);
        let mode = src.start(tx, mtx).unwrap();
        let samples = collect(&rx, 1_500);
        src.stop();
        eprintln!("mode {mode:?}, {} samples", samples.len());
        assert_eq!(mode, CaptureMode::TeamsOnly);
        assert!(samples.len() >= 16_000, "silence keeps the stream aligned");
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
            if let Ok(c) = tap.rx.recv_timeout(Duration::from_millis(50)) {
                raw.extend(c);
            }
        }
        drop(tap);
        let _ = play.kill();
        let _ = play.wait();
        let _ = std::fs::remove_file(&wav);
        let rms = (raw.iter().map(|x| x * x).sum::<f32>() / raw.len().max(1) as f32).sqrt();
        eprintln!("tap rate {rate} Hz, {} raw samples, rms {rms:.4}", raw.len());
        assert!(rms > 0.01, "heard afplay through its own process tap");
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
