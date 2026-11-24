//! Thread-safe process enumeration on macOS for the meeting threads (Teams audio tap, Teams window
//! reader). `NSWorkspace.runningApplications` is AppKit and only safe on the main thread, and those
//! threads poll it several times a second, so they use libproc instead: every pid
//! (`proc_listallpids`), its executable path (`proc_pidpath`) and, for an app's main executable, the
//! bundle id read from the app's Info.plist (CFBundle, thread-safe; cached per app path, a failed
//! lookup only briefly). Callers that poll often cache the pids they find (see `ax_macos.rs`).

use crate::watcher::ax_macos::{cf_to_string, CfGuard};
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

type CFTypeRef = *const c_void;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFURLCreateFromFileSystemRepresentation(alloc: CFTypeRef, buffer: *const u8, len: isize, is_directory: u8) -> CFTypeRef;
    fn CFBundleCreate(alloc: CFTypeRef, url: CFTypeRef) -> CFTypeRef;
    fn CFBundleGetIdentifier(bundle: CFTypeRef) -> CFTypeRef;
}

/// Every pid on the system (best effort; empty on failure).
pub fn all_pids() -> Vec<i32> {
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    // Room for processes started between the two calls.
    let mut buf = vec![0i32; n as usize + 64];
    let bytes = (buf.len() * std::mem::size_of::<i32>()) as i32;
    let n = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast(), bytes) };
    buf.truncate(n.clamp(0, buf.len() as i32) as usize);
    buf.retain(|p| *p > 0);
    buf
}

/// The executable path of `pid`.
pub fn pid_path(pid: i32) -> Option<String> {
    // On the stack: this runs for every pid on each scan.
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    std::str::from_utf8(buf.get(..n as usize)?).ok().map(str::to_owned)
}

/// The parent pid of `pid` (0 when unknown).
pub fn parent_pid(pid: i32) -> i32 {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&mut info as *mut libc::proc_bsdinfo).cast(), size) };
    if n == size { info.pbi_ppid as i32 } else { 0 }
}

/// The parent, grandparent, … of `pid`, at most `hops` of them, stopping at launchd.
pub fn ancestors(pid: i32, hops: usize) -> Vec<i32> {
    ancestors_with(pid, hops, parent_pid)
}

fn ancestors_with(pid: i32, hops: usize, parent: impl Fn(i32) -> i32) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur = pid;
    while out.len() < hops {
        let p = parent(cur);
        if p <= 1 || p == cur || out.contains(&p) {
            break;
        }
        out.push(p);
        cur = p;
    }
    out
}

/// The `.app` bundle whose *main* executable `exe` is (`…/X.app/Contents/MacOS/X`); `None` for helper
/// apps nested inside another bundle, command-line tools and anything else.
pub fn main_app_bundle(exe: &str) -> Option<&str> {
    let end = exe.find(".app/")? + ".app".len();
    let name = exe[end..].strip_prefix("/Contents/MacOS/")?;
    (!name.is_empty() && !name.contains('/')).then_some(&exe[..end])
}

/// How long a failed bundle-id lookup is remembered: an app caught mid-install or mid-update (Teams
/// updates itself in place) gets its id once it's readable again.
const MISS_TTL: Duration = Duration::from_secs(30);

/// A cached lookup, if still usable at `now`: hits forever, misses for [`MISS_TTL`].
fn cached(entry: Option<&(Option<String>, Instant)>, now: Instant) -> Option<Option<String>> {
    match entry? {
        (Some(id), _) => Some(Some(id.clone())),
        (None, at) if now.duration_since(*at) < MISS_TTL => Some(None),
        (None, _) => None,
    }
}

/// The bundle id of the app at `bundle_path` (cached).
fn bundle_id(bundle_path: &str) -> Option<String> {
    static CACHE: OnceLock<parking_lot::Mutex<HashMap<String, (Option<String>, Instant)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(hit) = cached(cache.lock().get(bundle_path), Instant::now()) {
        return hit;
    }
    let id = (|| {
        let url = CfGuard::new(unsafe {
            CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bundle_path.as_ptr(), bundle_path.len() as isize, 1)
        })?;
        let bundle = CfGuard::new(unsafe { CFBundleCreate(std::ptr::null(), url.as_ptr()) })?;
        // Get rule: owned by the bundle.
        cf_to_string(unsafe { CFBundleGetIdentifier(bundle.as_ptr()) })
    })();
    cache.lock().insert(bundle_path.to_string(), (id.clone(), Instant::now()));
    id
}

/// Pids of running apps (main executables of `.app` bundles) whose bundle id satisfies `matches`.
pub fn running_app_pids(matches: impl Fn(&str) -> bool) -> Vec<i32> {
    all_pids()
        .into_iter()
        .filter(|&pid| {
            pid_path(pid)
                .and_then(|path| main_app_bundle(&path).and_then(bundle_id))
                .is_some_and(|id| matches(&id))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_app_bundle_only_for_main_executables() {
        assert_eq!(
            main_app_bundle("/Applications/Microsoft Teams.app/Contents/MacOS/MSTeams"),
            Some("/Applications/Microsoft Teams.app")
        );
        // Helpers nested inside the app are not the app.
        assert_eq!(
            main_app_bundle(
                "/Applications/Microsoft Teams.app/Contents/Helpers/Microsoft Teams WebView.app/Contents/Frameworks/Microsoft Teams WebView Helper (Renderer).app/Contents/MacOS/Microsoft Teams WebView Helper (Renderer)"
            ),
            None
        );
        assert_eq!(main_app_bundle("/usr/bin/afplay"), None);
        assert_eq!(main_app_bundle("/Applications/Foo.app/Contents/MacOS/sub/tool"), None);
        assert_eq!(main_app_bundle("/Applications/Foo.app/Contents/MacOS/"), None);
    }

    #[test]
    fn failed_bundle_lookups_expire_hits_dont() {
        let t0 = Instant::now();
        let hit = (Some("com.microsoft.teams2".to_string()), t0);
        let miss = (None, t0);
        assert_eq!(cached(None, t0), None);
        assert_eq!(cached(Some(&hit), t0 + Duration::from_secs(3_600)), Some(Some("com.microsoft.teams2".into())));
        assert_eq!(cached(Some(&miss), t0 + Duration::from_secs(5)), Some(None));
        assert_eq!(cached(Some(&miss), t0 + MISS_TTL), None, "looked up again");
    }

    #[test]
    fn ancestors_stop_at_launchd_loops_and_hop_limit() {
        let tree = |p: i32| match p {
            40 => 30,
            30 => 20,
            20 => 10,
            10 => 1,
            7 => 7,
            _ => 0,
        };
        assert_eq!(ancestors_with(40, 4, tree), vec![30, 20, 10]);
        assert_eq!(ancestors_with(40, 2, tree), vec![30, 20]);
        assert!(ancestors_with(7, 4, tree).is_empty());
        assert!(ancestors_with(99, 4, tree).is_empty());
    }

    #[test]
    fn finds_this_test_process_tree_and_a_running_app() {
        let me = std::process::id() as i32;
        assert!(all_pids().contains(&me));
        assert!(pid_path(me).is_some());
        assert!(!ancestors(me, 4).is_empty());
        // Finder (com.apple.finder) runs in every logged-in session.
        if !running_app_pids(|id| id == "com.apple.finder").is_empty() {
            assert!(running_app_pids(|id| id == "com.apple.finder").iter().all(|p| *p > 0));
        }
    }
}
