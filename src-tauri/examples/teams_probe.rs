//! Dump the Teams meeting window's accessibility tree to JSON (meetings plan Task 4 / Task 14).
//!
//!   cargo run --example teams_probe -- --out tests/fixtures/teams/win_call_1.json [--any] [--parse]
//!
//! `--any` dumps the first Teams window even if it isn't a meeting (e.g. the idle main window).
//! `--parse` also prints what Wren's parser reads from it.
//! Dumps contain participant names and meeting titles: redact them before committing.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)).cloned();
    let any = args.iter().any(|a| a == "--any");
    let show = args.iter().any(|a| a == "--parse");
    #[cfg(windows)]
    {
        use wren_lib::meetings::teams::{snapshot::Dump, uia::UiaTeamsReader, TeamsReader};
        let _com = wren_lib::watcher::uia::ComMta::join();
        let mut reader = UiaTeamsReader::new().expect("UI Automation");
        reader.any_window = any;
        let t0 = std::time::Instant::now();
        let Some((tree, title)) = reader.snapshot() else {
            eprintln!("no Teams {}window found", if any { "" } else { "meeting " });
            std::process::exit(2);
        };
        eprintln!("read {} nodes in {} ms", tree.count(), t0.elapsed().as_millis());
        if show {
            println!("{:#?}", wren_lib::meetings::teams::parse(&tree, &title));
        }
        let dump = Dump { title, tree };
        let json = serde_json::to_string_pretty(&dump).unwrap();
        match out {
            Some(p) => std::fs::write(&p, json).expect("write"),
            None => println!("{json}"),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (out, any, show);
        eprintln!("macOS probe: see meetings::teams::ax_macos (plan Task 6)");
    }
}
