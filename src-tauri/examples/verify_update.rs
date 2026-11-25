//! Release check: would an installed Wren from this build accept this update file?
//!
//!   cargo run --release --example verify_update -- <installer or .app.tar.gz> <its .sig> <version>
//!
//! Uses the app's own update source (`WREN_UPDATE_REPO` / `WREN_UPDATE_PUBKEY`, read at build time
//! exactly as for wren.exe) and its own `updater::verify_signature`, so a release whose signing key
//! does not match the key built into the app fails here instead of on users' machines. Exit 0 =
//! accepted, 1 = refused, 2 = bad arguments. A build with no update source (`none`) has nothing to
//! verify and exits 0.
use wren_lib::updater::{update_source, verify_signature};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [file, sig, version] = args.as_slice() else {
        eprintln!("usage: verify_update <file> <file.sig> <version>");
        std::process::exit(2);
    };
    let Some(source) = update_source() else {
        println!("UPDATE SIGNATURE SKIPPED: this build has no update source (WREN_UPDATE_REPO=none)");
        return;
    };
    let payload = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("UPDATE SIGNATURE FAIL: cannot read {file} ({e})");
            std::process::exit(1);
        }
    };
    let signature = match std::fs::read_to_string(sig) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("UPDATE SIGNATURE FAIL: cannot read {sig} ({e})");
            std::process::exit(1);
        }
    };
    match verify_signature(&source.public_key_b64, &payload, signature.trim(), version) {
        Ok(()) => println!(
            "UPDATE SIGNATURE OK: {file} (version {version}) verifies with the key built in for {}/{}",
            source.owner, source.repo
        ),
        Err(e) => {
            eprintln!(
                "UPDATE SIGNATURE FAIL: {file} does not verify with the key built in for {}/{}: {e}",
                source.owner, source.repo
            );
            std::process::exit(1);
        }
    }
}
