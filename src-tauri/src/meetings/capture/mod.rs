//! Audio capture for meetings (spec §5.1, plan Tasks 5–7): the microphone ("Me") and Teams' audio
//! ("Them"), each delivered as 16 kHz mono blocks stamped with a wall-clock-aligned stream position.

use crate::meetings::CaptureMode;
use crossbeam_channel::Sender;

#[cfg(target_os = "macos")]
pub mod mac_tap;
pub mod wav;
#[cfg(windows)]
pub mod win_loopback;

/// 16 kHz mono samples starting at `stream_pos_ms` (ms since the source started, wall-clock aligned:
/// gaps where nothing played are filled with silence so both streams line up).
#[derive(Debug, Clone, PartialEq)]
pub struct AudioBlock {
    pub samples: Vec<f32>,
    pub stream_pos_ms: u64,
}

/// A capture source. `start` returns how it is capturing; changes later (Teams appearing, restarting
/// or failing) are reported through `mode_tx`.
pub trait AudioSource: Send {
    fn start(&mut self, tx: Sender<AudioBlock>, mode_tx: Sender<CaptureMode>) -> anyhow::Result<CaptureMode>;
    fn stop(&mut self);
}

/// Pick the root Teams processes from `(pid, parent_pid, exe name)`: `ms-teams.exe` processes whose
/// parent isn't another Teams process. Capturing a root with its process tree covers its helpers.
pub fn teams_roots(procs: &[(u32, u32, String)]) -> Vec<u32> {
    let is_teams = |name: &str| name.eq_ignore_ascii_case("ms-teams.exe");
    let teams: Vec<u32> = procs.iter().filter(|p| is_teams(&p.2)).map(|p| p.0).collect();
    let mut roots: Vec<u32> = procs
        .iter()
        .filter(|p| is_teams(&p.2) && !teams.contains(&p.1))
        .map(|p| p.0)
        .collect();
    roots.sort_unstable();
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, name: &str) -> (u32, u32, String) {
        (pid, ppid, name.to_string())
    }

    #[test]
    fn teams_roots_skips_children_and_other_apps() {
        let procs = [
            p(25768, 9260, "ms-teams.exe"),
            p(15796, 25768, "ms-teams.exe"),
            p(26720, 25768, "msedgewebview2.exe"),
            p(500, 4, "chrome.exe"),
        ];
        assert_eq!(teams_roots(&procs), vec![25768]);
    }

    #[test]
    fn teams_roots_empty_when_teams_closed() {
        assert!(teams_roots(&[p(500, 4, "chrome.exe")]).is_empty());
    }

    #[test]
    fn teams_roots_case_insensitive() {
        assert_eq!(teams_roots(&[p(7, 1, "MS-Teams.EXE")]), vec![7]);
    }
}
