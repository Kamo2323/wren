//! Live check of moving a key out of the real OS credential store into Wren's key file, using a
//! throwaway entry (service "Wren-key-move-probe") and a temp folder. Never touches Wren's own key.
//! The entry is created by this same binary, so macOS doesn't prompt.
//!
//!   cargo run --example key_move_probe

use std::path::PathBuf;
use wren_lib::claude_key_file::{KeyFile, LegacyRead, KEY_FILE};

const SERVICE: &str = "Wren-key-move-probe";
const ACCOUNT: &str = "probe";
const FAKE: &str = "sk-ant-probe-not-a-real-key-0123456789abcdef";

fn entry() -> keyring::Entry {
    keyring::Entry::new(SERVICE, ACCOUNT).expect("keyring entry")
}

fn read() -> LegacyRead {
    match entry().get_password() {
        Ok(k) if !k.trim().is_empty() => LegacyRead::Found(k),
        Ok(_) | Err(keyring::Error::NoEntry) => LegacyRead::Missing,
        Err(e) => {
            eprintln!("read failed: {e}");
            LegacyRead::Failed
        }
    }
}

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = entry().delete_credential();
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn main() {
    let dir = std::env::temp_dir().join(format!("wren-key-move-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _cleanup = Cleanup(dir.clone());

    entry().set_password(FAKE).expect("create the probe entry");
    let created = matches!(read(), LegacyRead::Found(_));
    println!("probe entry created: {created}");
    assert!(created, "probe entry wasn't readable after creation");

    let kf = KeyFile::new(&dir);
    let got = kf.get_or_move(read, || entry().delete_credential().expect("delete the probe entry"));
    assert_eq!(got.as_deref(), Some(FAKE), "moved key mismatch");
    println!("moved key matches: true");

    let on_disk = std::fs::read_to_string(dir.join(KEY_FILE)).ok();
    assert_eq!(on_disk.as_deref(), Some(FAKE), "key file content mismatch");
    println!("file matches: true");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode() & 0o777;
        println!("file mode: {mode:o}");
        assert_eq!(mode, 0o600, "expected mode 600, got {mode:o}");
    }

    let gone = read() == LegacyRead::Missing;
    println!("probe entry gone from the store: {gone}");
    assert!(gone, "probe entry still present in OS store after move");

    let again = kf.get_or_move(|| panic!("store read a second time"), || panic!("second delete"));
    assert_eq!(again.as_deref(), Some(FAKE), "second read mismatch");
    println!("second read from the file only: true");

    drop(_cleanup);
    println!("cleaned up");
}
