//! Save a Claude API key into the OS credential store (the same place Settings › Meeting notes saves it),
//! after checking it with a tiny request. The key is read from a file so it never appears in a command line.
//!
//!   cargo run --example claude_key_set -- <file containing the key>

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: claude_key_set <key file>");
        std::process::exit(2);
    };
    let key = std::fs::read_to_string(&path).unwrap_or_default().trim().to_string();
    if !wren_lib::meetings::claude::looks_like_key(&key) {
        eprintln!("that doesn't look like a Claude API key");
        std::process::exit(3);
    }
    match wren_lib::meetings::claude::test_key(&key) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("key check failed: {e}");
            std::process::exit(4);
        }
    }
    wren_lib::meetings::claude::KeyStore::system().set(&key).expect("save");
    println!("key checked and saved ({} chars)", key.len());
}
