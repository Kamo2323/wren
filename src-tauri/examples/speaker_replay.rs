//! Speaker-naming replay benchmark (see docs/meetings-speaker-test-call.md).
//!
//!   cargo run --example speaker_replay -- fixtures
//!       Score the bundled synthetic fixtures (tests/fixtures/speaker-replay/*).
//!   cargo run --example speaker_replay -- score <call-diagnostics.jsonl> <case.json> [--lag MS]
//!       Score one recorded call against its answer key.
//!   cargo run --example speaker_replay -- extract <call-diagnostics.jsonl>
//!       Print a case.json skeleton (the call's Them lines, answers empty) to fill in.
//!
//! Replays the Teams readings through the app's real `SpeakerTimeline::attribute`; no audio or engine.

use std::path::{Path, PathBuf};
use wren_lib::meetings::diagnostics;
use wren_lib::meetings::replay::{self, Case};

fn load(diag: &Path, case: Option<&Path>) -> Result<(diagnostics::Recorded, Option<Case>), String> {
    let text = std::fs::read_to_string(diag).map_err(|e| format!("{}: {e}", diag.display()))?;
    let rec = diagnostics::parse(&text).map_err(|e| format!("{}: {e}", diag.display()))?;
    let case = match case {
        Some(p) => {
            let t = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
            Some(serde_json::from_str(&t).map_err(|e| format!("{}: {e}", p.display()))?)
        }
        None => None,
    };
    Ok((rec, case))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let lag = args.iter().position(|a| a == "--lag").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<u64>().ok());
    let pos: Vec<&String> = {
        let mut skip = false;
        args.iter()
            .filter(|a| {
                if skip {
                    skip = false;
                    return false;
                }
                if *a == "--lag" {
                    skip = true;
                    return false;
                }
                true
            })
            .collect()
    };
    let result = match pos.first().map(|s| s.as_str()) {
        Some("fixtures") => {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/speaker-replay");
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
                .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect())
                .unwrap_or_default();
            dirs.sort();
            dirs.iter().try_for_each(|d| {
                let (rec, case) = load(&d.join("call-diagnostics.jsonl"), Some(&d.join("case.json")))?;
                println!("{}", replay::run(&rec.readings, &case.unwrap_or_default(), lag));
                Ok::<(), String>(())
            })
        }
        Some("score") if pos.len() >= 3 => load(Path::new(pos[1]), Some(Path::new(pos[2]))).map(|(rec, case)| {
            if !rec.header.platform.is_empty() {
                println!("Recorded on {} ({} Teams readings)", rec.header.platform, rec.readings.len());
            }
            println!("{}", replay::run(&rec.readings, &case.unwrap_or_default(), lag));
        }),
        Some("extract") if pos.len() >= 2 => load(Path::new(pos[1]), None).map(|(rec, _)| {
            println!("{}", serde_json::to_string_pretty(&replay::skeleton(&rec)).unwrap_or_default());
        }),
        _ => Err("usage: speaker_replay fixtures | score <call-diagnostics.jsonl> <case.json> [--lag MS] | extract <call-diagnostics.jsonl>".into()),
    };
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
