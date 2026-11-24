//! Turns a Teams meeting-window tree into a [`TeamsObservation`] (plan Task 4). Pure.
//!
//! PROVISIONAL RULES. They come from what Granola's Teams helper looks for (`hangup-button`,
//! "Active speaker", "You are speaking", "(You)") and new Teams' aria labels. Task 14 replaces the
//! synthetic tests with real captures from a call and edits the rule table below — keep every rule here.

use crate::meetings::speakers::{strip_self_suffix, TeamsObservation};
use crate::meetings::teams::snapshot::UiNode;

// ---- rule table ----

/// AutomationIds (Teams `data-tid`) that exist only in a meeting window.
const MEETING_MARKERS: &[&str] = &["hangup-button", "call-hangup", "hangup-btn"];
/// States Teams appends to a participant's label: "<Name>, <state>, <state>".
const TILE_STATES: &[&str] = &[
    "active speaker", "speaking", "muted", "unmuted", "video on", "video off", "camera on", "camera off",
    "organizer", "organiser", "presenter", "attendee", "guest", "external", "hand raised", "pinned",
    "spotlighted", "computer audio muted", "computer audio unmuted", "telephone muted",
    "telephone unmuted", "sharing", "screen sharing", "in the lobby", "mic muted", "mic unmuted",
    "video is on", "video is off",
];
/// First part of the user's own video tile label (seen live: "Myself video, <Name>, Unmuted, video is on, …").
const SELF_TILE_PREFIX: &str = "myself video";
/// Tile states that mean "this person is talking".
const SPEAKING_STATES: &[&str] = &["active speaker", "speaking"];
/// Suffix of a help/name text meaning the named person is talking ("Tom Reid is speaking").
const IS_SPEAKING_SUFFIX: &str = " is speaking";
/// The mic toggle: matched by AutomationId fragment, or by a Button whose name mentions the mic.
const MIC_BUTTON_IDS: &[&str] = &["microphone-button", "mic-button", "microphone"];
/// Window-title pages that are not a meeting name.
const NON_MEETING_TITLES: &[&str] =
    &["", "meeting", "microsoft teams", "calendar", "chat", "activity", "teams", "calls", "meet now"];
const TEAMS_SUFFIX: &str = "Microsoft Teams";
/// Title parts naming a Teams window layout rather than the meeting: the Mac's call window, while
/// another app is in front, is titled "Meeting compact view | Meeting with Sarah Jones".
const VIEW_TITLE_PARTS: &[&str] = &["meeting compact view", "compact view"];

// ---- parser ----

pub fn is_meeting_window(root: &UiNode) -> bool {
    root.walk().iter().any(|n| MEETING_MARKERS.iter().any(|m| n.automation_id.eq_ignore_ascii_case(m)))
}

/// `"Weekly sync | Microsoft Teams"` → `Some("Weekly sync")`, `"Meeting compact view | Weekly sync"`
/// (macOS) → `Some("Weekly sync")`; Teams' own page names → `None`.
pub fn clean_title(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw
        .split(" | ")
        .map(str::trim)
        .filter(|p| !VIEW_TITLE_PARTS.contains(&p.to_ascii_lowercase().as_str()))
        .collect();
    let parts: &[&str] = match parts.split_last() {
        Some((last, rest)) if last.eq_ignore_ascii_case(TEAMS_SUFFIX) => rest,
        _ => &parts,
    };
    if parts.len() != 1 {
        return None;
    }
    let t = parts[0].trim();
    let lower = t.to_ascii_lowercase();
    if NON_MEETING_TITLES.contains(&lower.as_str()) || lower.starts_with("meeting in \"") || t.chars().count() > 120 {
        return None;
    }
    Some(t.to_string())
}

/// A participant label: `"Sarah Jones, Active speaker, Unmuted, Has context menu"` →
/// `("Sarah Jones", ["active speaker", "unmuted", …], false)`; the user's own tile
/// (`"Myself video, Max, Unmuted, …"`) → `("Max", …, true)`. At least one known participant state is
/// required; unknown states ("fill frame", "has context menu") are kept but don't count.
fn tile_label(label: &str) -> Option<(String, Vec<String>, bool)> {
    let mut parts: Vec<&str> = label.split(',').map(str::trim).collect();
    let mine = parts.first().is_some_and(|p| p.eq_ignore_ascii_case(SELF_TILE_PREFIX));
    if mine {
        parts.remove(0);
    }
    let (name, rest) = parts.split_first()?;
    let name = name.trim();
    let states: Vec<String> = rest.iter().map(|s| s.to_ascii_lowercase()).collect();
    if name.is_empty() || name.len() > 80 || name.contains('|') || TILE_STATES.contains(&name.to_ascii_lowercase().as_str()) {
        return None;
    }
    let self_marked = strip_self_suffix(name).len() != name.len();
    if states.is_empty() {
        // A bare name counts only when Teams marks it as the user ("Max (You)").
        return self_marked.then(|| (name.to_string(), states, false));
    }
    states.iter().any(|s| TILE_STATES.contains(&s.as_str())).then(|| (name.to_string(), states, mine))
}

fn push_unique(v: &mut Vec<String>, s: &str) {
    if !v.iter().any(|x| x.eq_ignore_ascii_case(s)) {
        v.push(s.to_string());
    }
}

fn mic_muted(n: &UiNode) -> Option<bool> {
    let aid = n.automation_id.to_ascii_lowercase();
    let name = n.name.to_ascii_lowercase();
    let is_mic = MIC_BUTTON_IDS.iter().any(|m| aid.contains(m))
        || (n.role.contains("Button") && (name.starts_with("mic") || name.contains(" mic") || name.contains("microphone")));
    if !is_mic {
        return None;
    }
    if name.starts_with("unmute") {
        return Some(true);
    }
    if name.starts_with("mute") {
        return Some(false);
    }
    if n.states.iter().any(|s| s == "toggled_off") {
        return Some(true);
    }
    if n.states.iter().any(|s| s == "toggled_on") {
        return Some(false);
    }
    None
}

/// Read one Teams window. `at_ms` is left at 0 for the caller to fill.
pub fn parse(root: &UiNode, window_title: &str) -> TeamsObservation {
    if !is_meeting_window(root) {
        return TeamsObservation::default();
    }
    let mut o = TeamsObservation { title: clean_title(window_title), ..Default::default() };
    let mut own_tile_muted = None;
    for n in root.walk() {
        if let Some((name, states, mine)) = tile_label(&n.name) {
            if mine {
                o.self_name = Some(name.clone());
                push_unique(&mut o.roster, &format!("{name} (You)"));
                if states.iter().any(|s| s == "muted" || s == "mic muted") {
                    own_tile_muted = Some(true);
                } else if states.iter().any(|s| s == "unmuted" || s == "mic unmuted") {
                    own_tile_muted = Some(false);
                }
            } else {
                push_unique(&mut o.roster, &name);
                if states.iter().any(|s| SPEAKING_STATES.contains(&s.as_str())) {
                    push_unique(&mut o.speaking, &name);
                }
            }
        }
        for text in [&n.help, &n.name] {
            if let Some(who) = text.trim().strip_suffix(IS_SPEAKING_SUFFIX) {
                let who = who.trim();
                if !who.is_empty() && who.len() <= 80 && !who.eq_ignore_ascii_case("you") {
                    push_unique(&mut o.speaking, who);
                }
            }
        }
        if o.self_muted.is_none() {
            o.self_muted = mic_muted(n);
        }
    }
    if o.self_name.is_none() {
        o.self_name = o
            .roster
            .iter()
            .find(|r| strip_self_suffix(r).len() != r.trim().len())
            .map(|r| strip_self_suffix(r).to_string());
    }
    if o.self_muted.is_none() {
        o.self_muted = own_tile_muted;
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::teams::snapshot::UiNode;

    fn n(role: &str, name: &str, aid: &str, children: Vec<UiNode>) -> UiNode {
        UiNode {
            role: role.into(),
            name: name.into(),
            automation_id: aid.into(),
            children,
            ..Default::default()
        }
    }

    /// A meeting window modelled on new Teams (WebView2): toolbar with hang-up and mic buttons,
    /// participant tiles named "<Name>, <states>".
    fn meeting(mic_label: &str, tiles: &[&str]) -> UiNode {
        let tiles = tiles.iter().map(|t| n("ListItem", t, "", vec![])).collect();
        n(
            "Window",
            "Weekly sync | Microsoft Teams",
            "",
            vec![
                n("Group", "", "calling-stage", vec![n("List", "", "", tiles)]),
                n(
                    "ToolBar",
                    "Meeting controls",
                    "",
                    vec![
                        n("Button", mic_label, "microphone-button", vec![]),
                        n("Button", "Leave (Ctrl+Shift+H)", "hangup-button", vec![]),
                    ],
                ),
            ],
        )
    }

    #[test]
    fn speaking_tile_detected() {
        let t = meeting(
            "Mute mic (Ctrl+Shift+M)",
            &["Sarah Jones, Active speaker, Unmuted", "Tom Reid, Muted", "Max Teams (You), Unmuted"],
        );
        let o = parse(&t, "Weekly sync | Microsoft Teams");
        assert_eq!(o.speaking, vec!["Sarah Jones".to_string()]);
    }

    #[test]
    fn speaking_via_help_text_and_lowercase_variant() {
        let mut t = meeting("Mute mic", &["Tom Reid, Unmuted"]);
        t.children[0].children[0].children[0].help = "Tom Reid is speaking".into();
        let o = parse(&t, "x | Microsoft Teams");
        assert_eq!(o.speaking, vec!["Tom Reid".to_string()]);
    }

    #[test]
    fn roster_names_collected() {
        let t = meeting("Mute mic", &["Sarah Jones, Unmuted", "Tom Reid, Muted, Video off", "Max Teams (You), Unmuted"]);
        let o = parse(&t, "x | Microsoft Teams");
        assert_eq!(o.roster, vec!["Sarah Jones", "Tom Reid", "Max Teams (You)"]);
    }

    #[test]
    fn self_entry_detected_by_you_suffix() {
        let t = meeting("Mute mic", &["Sarah Jones, Unmuted", "Max Teams (You), Unmuted"]);
        assert_eq!(parse(&t, "x").self_name.as_deref(), Some("Max Teams"));
        let t = meeting("Mute mic", &["Max Teams (Me)"]);
        assert_eq!(parse(&t, "x").self_name.as_deref(), Some("Max Teams"));
    }

    #[test]
    fn self_muted_from_mic_button() {
        assert_eq!(parse(&meeting("Unmute mic (Ctrl+Shift+M)", &[]), "x").self_muted, Some(true));
        assert_eq!(parse(&meeting("Mute mic (Ctrl+Shift+M)", &[]), "x").self_muted, Some(false));
        assert_eq!(parse(&meeting("Mic", &[]), "x").self_muted, None, "unknown label");
    }

    #[test]
    fn self_muted_from_toggle_state() {
        let mut t = meeting("Mic", &[]);
        t.children[1].children[0].states = vec!["toggled_off".into()];
        assert_eq!(parse(&t, "x").self_muted, Some(true));
        t.children[1].children[0].states = vec!["toggled_on".into()];
        assert_eq!(parse(&t, "x").self_muted, Some(false));
    }

    #[test]
    fn title_cleaned() {
        assert_eq!(clean_title("Weekly sync | Microsoft Teams").as_deref(), Some("Weekly sync"));
        assert_eq!(clean_title("Meeting with Sarah Jones | Microsoft Teams").as_deref(), Some("Meeting with Sarah Jones"));
        assert_eq!(clean_title("Microsoft Teams"), None);
        assert_eq!(clean_title("Meeting | Microsoft Teams"), None, "generic");
        assert_eq!(clean_title("Meeting in \"General\" | Microsoft Teams"), None, "channel boilerplate");
        assert_eq!(clean_title("Chat | Sarah Jones | Microsoft Teams"), None, "not a meeting window title");
        // macOS: the call window while another app is in front.
        assert_eq!(clean_title("Meeting compact view | Meeting with Sarah Jones").as_deref(), Some("Meeting with Sarah Jones"));
        assert_eq!(clean_title("Meeting compact view | Weekly sync").as_deref(), Some("Weekly sync"));
        assert_eq!(clean_title("Meeting compact view"), None);
        assert_eq!(clean_title("Meeting compact view | Meeting"), None, "generic");
        let o = parse(&meeting("Mute mic", &[]), "Weekly sync | Microsoft Teams");
        assert_eq!(o.title.as_deref(), Some("Weekly sync"));
    }

    #[test]
    fn main_window_yields_empty_observation() {
        let main = n(
            "Window",
            "Chat | Sarah Jones | Microsoft Teams",
            "",
            vec![n("ListItem", "Sarah Jones, Active speaker", "", vec![])],
        );
        let o = parse(&main, "Chat | Sarah Jones | Microsoft Teams");
        assert!(!is_meeting_window(&main));
        assert!(o.speaking.is_empty() && o.roster.is_empty() && o.self_muted.is_none() && o.title.is_none());
    }

    #[test]
    fn real_call_window_reads_self_mute_and_title() {
        // Real dump from a Teams call on this PC (only the user in the call), name redacted.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/teams/win_call_self.json");
        let dump: crate::meetings::teams::snapshot::Dump = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(is_meeting_window(&dump.tree));
        let o = parse(&dump.tree, &dump.title);
        assert_eq!(o.self_name.as_deref(), Some("Alex Example"));
        assert_eq!(o.self_muted, Some(false));
        assert_eq!(o.title.as_deref(), Some("Meeting with Alex Example"));
        assert!(o.speaking.is_empty());
    }

    #[test]
    fn mac_real_call_window_reads_self_mute_and_title() {
        // Real dump from a Teams call on the Mac (only the user in the call, muted, Wren in front so
        // Teams shows its compact view), name redacted.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/teams/mac_call_self.json");
        let dump: crate::meetings::teams::snapshot::Dump = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(is_meeting_window(&dump.tree));
        let o = parse(&dump.tree, &dump.title);
        assert_eq!(o.self_name.as_deref(), Some("Alex Example"));
        assert_eq!(o.self_muted, Some(true));
        assert_eq!(o.title.as_deref(), Some("Meeting with Alex Example"));
        assert!(o.speaking.is_empty());
    }

    #[test]
    fn myself_video_tile_and_extra_states() {
        let t = meeting(
            "Unmute mic",
            &[
                "Myself video, Max Teams, Muted, video is on, Fill frame, Has context menu",
                "Sarah Jones, Unmuted, video is off, Has context menu",
                "Tom Reid, Speaking, Unmuted, video is on, Has context menu",
                "Fill frame, Has context menu",
            ],
        );
        let o = parse(&t, "x");
        assert_eq!(o.self_name.as_deref(), Some("Max Teams"));
        assert_eq!(o.self_muted, Some(true));
        assert_eq!(o.roster, vec!["Max Teams (You)", "Sarah Jones", "Tom Reid"]);
        assert_eq!(o.speaking, vec!["Tom Reid".to_string()]);
    }

    #[test]
    fn idle_window_has_no_speakers() {
        // Real dump of this PC's Teams main window (not in a call), captured with `teams_probe`.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/teams/win_idle.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            panic!("missing fixture {path}: run `cargo run --example teams_probe -- --out {path}`");
        };
        let dump: crate::meetings::teams::snapshot::Dump = serde_json::from_str(&text).unwrap();
        assert!(dump.tree.count() > 10, "a real tree");
        let o = parse(&dump.tree, &dump.title);
        assert!(o.speaking.is_empty());
        assert!(o.title.is_none());
    }

    #[test]
    fn mac_idle_window_has_no_speakers() {
        // Real dump of the Mac's Teams main window (not in a call), captured with `teams_probe --any`.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/teams/mac_idle.json");
        let Ok(text) = std::fs::read_to_string(path) else {
            panic!("missing fixture {path}: run `cargo run --example teams_probe -- --any --out {path}`");
        };
        let dump: crate::meetings::teams::snapshot::Dump = serde_json::from_str(&text).unwrap();
        assert!(dump.tree.count() > 10, "a real tree");
        assert!(!is_meeting_window(&dump.tree));
        let o = parse(&dump.tree, &dump.title);
        assert!(o.speaking.is_empty());
        assert!(o.title.is_none());
    }
}
