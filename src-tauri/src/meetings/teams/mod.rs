//! Reading the Teams meeting window (meetings spec §5.3, plan Task 4): who is speaking, the roster, the
//! user's own entry and mute state, and the meeting title. OS readers copy the window's accessibility
//! tree into a [`snapshot::UiNode`]; [`parse::parse`] turns it into a [`TeamsObservation`].
//!
//! Privacy: names and titles read here go to the meeting only; they are never logged.

#[cfg(target_os = "macos")]
pub mod ax_macos;
pub mod parse;
pub mod snapshot;
#[cfg(windows)]
pub mod uia;

pub use parse::{clean_title, is_meeting_window, parse};

use crate::meetings::speakers::TeamsObservation;
use crossbeam_channel::Sender;
use snapshot::UiNode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Limits on one snapshot (spec: depth ≤ 40, ≤ 5,000 nodes, ~150 ms).
pub const MAX_DEPTH: usize = 40;
pub const MAX_NODES: usize = 5_000;
pub const SNAPSHOT_BUDGET: Duration = Duration::from_millis(150);

/// Finds the Teams meeting window and copies its tree.
pub trait TeamsReader {
    /// `(tree, window title)` of the Teams meeting window, or `None` when there is none.
    fn snapshot(&mut self) -> Option<(UiNode, String)>;
}

/// Factory that builds a reader on the watcher thread (COM objects can't cross threads).
pub type ReaderFactory = Box<dyn FnOnce() -> Option<Box<dyn TeamsReader>> + Send>;

/// Poll a reader (built on the watcher thread by `make`) every `interval` until `stop`, sending one
/// observation per poll. `clock` gives the meeting-stream position in ms (so observations line up with
/// audio segments).
pub fn spawn_watcher(
    make: ReaderFactory,
    interval: Duration,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    tx: Sender<TeamsObservation>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name("wren-teams-watch".into()).spawn(move || {
        #[cfg(windows)]
        let _com = crate::watcher::uia::ComMta::join();
        let Some(mut reader) = make() else {
            tracing::warn!("Teams window reader unavailable; speaker names will be missing");
            return;
        };
        let mut slow_logged = false;
        while !stop.load(Ordering::Relaxed) {
            let t0 = Instant::now();
            let before = clock();
            let snap = reader.snapshot();
            // A tree walk can take a few hundred ms: stamp the read at its middle, not its start.
            let at_ms = before + (clock().saturating_sub(before)) / 2;
            let obs = match snap {
                Some((tree, title)) => {
                    let mut o = parse(&tree, &title);
                    o.at_ms = at_ms;
                    o
                }
                None => TeamsObservation { at_ms, ..Default::default() },
            };
            if tx.send(obs).is_err() {
                break;
            }
            let took = t0.elapsed();
            if took > SNAPSHOT_BUDGET * 3 && !slow_logged {
                tracing::info!(ms = took.as_millis() as u64, "Teams window read is slow");
                slow_logged = true;
            }
            std::thread::sleep(interval.saturating_sub(took));
        }
    })
}
