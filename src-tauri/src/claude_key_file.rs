//! The Claude API key's home on macOS: a private file in Wren's folder, not the login Keychain.
//!
//! Why: Mac releases are signed with Wren's own (self-signed) certificate, which has no Apple team.
//! macOS remembers a Keychain "Always Allow" for such an app by its exact build (`cdhash:` partition),
//! so every update asked for the login password again. A file readable only by the user's account
//! (mode 0600) never prompts. The first read after updating moves the key out of the Keychain (one
//! last prompt), then deletes the Keychain copy.
//!
//! Platform-independent so the move is tested everywhere; only macOS uses it (Windows keeps
//! Credential Manager, which has no prompts).

use std::io::Write;
use std::path::{Path, PathBuf};

/// The key file, in Wren's settings folder (`~/Library/Application Support/Wren` on a Mac).
pub const KEY_FILE: &str = "claude-api-key";
/// Written once the Keychain has been dealt with (key moved, or none there), so the Keychain is
/// never read again: no prompt, and an old Keychain copy can't come back after "Forget key".
pub const MOVED_MARKER: &str = "claude-api-key.moved-from-keychain";

/// What the old store said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyRead {
    Found(String),
    /// No key there.
    Missing,
    /// Couldn't read it (prompt denied, Keychain locked…): try again next launch.
    Failed,
}

pub struct KeyFile {
    dir: PathBuf,
}

impl KeyFile {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self) -> PathBuf {
        self.dir.join(KEY_FILE)
    }

    fn marker(&self) -> PathBuf {
        self.dir.join(MOVED_MARKER)
    }

    /// The saved key; `None` if there's no file or it's empty.
    pub fn read(&self) -> Option<String> {
        let s = std::fs::read_to_string(self.path()).ok()?;
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// Save the key (owner-only, written whole then renamed into place) and mark the Keychain done.
    pub fn write(&self, key: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{KEY_FILE}.{}.tmp", uuid::Uuid::new_v4()));
        let res = (|| -> std::io::Result<()> {
            let mut f = private_create(&tmp)?;
            f.write_all(key.trim().as_bytes())?;
            f.sync_all()?;
            drop(f);
            std::fs::rename(&tmp, self.path())?;
            Ok(())
        })();
        if res.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        res?;
        self.mark_moved();
        Ok(())
    }

    /// Delete the key. The marker stays, so an old Keychain copy is never read back.
    pub fn remove(&self) -> std::io::Result<()> {
        self.mark_moved_result()?;
        match std::fs::remove_file(self.path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    pub fn moved(&self) -> bool {
        self.marker().exists()
    }

    fn mark_moved_result(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::write(self.marker(), b"")
    }

    fn mark_moved(&self) {
        if let Err(e) = self.mark_moved_result() {
            tracing::warn!(error = %e, "couldn't write the Keychain move marker");
        }
    }

    /// The key: from the file, or — once — moved out of the old store. `delete_legacy` runs only
    /// after the file is safely written.
    pub fn get_or_move(&self, read_legacy: impl FnOnce() -> LegacyRead, delete_legacy: impl FnOnce()) -> Option<String> {
        if let Some(key) = self.read() {
            return Some(key);
        }
        if self.moved() {
            return None;
        }
        match read_legacy() {
            LegacyRead::Found(key) => {
                let key = key.trim().to_string();
                if key.is_empty() {
                    self.mark_moved();
                    return None;
                }
                match self.write(&key) {
                    Ok(()) => {
                        delete_legacy();
                        tracing::info!("Claude key moved from the Keychain to Wren's key file");
                    }
                    // Keep the Keychain copy; the move is tried again next launch.
                    Err(e) => tracing::warn!(error = %e, "couldn't write Wren's key file; key left in the Keychain"),
                }
                Some(key)
            }
            LegacyRead::Missing => {
                self.mark_moved();
                None
            }
            LegacyRead::Failed => {
                tracing::info!("couldn't read the Claude key from the Keychain; will try again next launch");
                None
            }
        }
    }
}

/// Create a brand-new file only the owner can read or write.
fn private_create(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let d = std::env::temp_dir().join(format!("wren-keyfile-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&d).unwrap();
            Self(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const KEY: &str = "sk-ant-api03-test-key-0123456789abcdefghijklmnop";

    fn never() -> LegacyRead {
        panic!("the Keychain must not be read again")
    }

    fn has_tmp_files(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
    }

    #[test]
    fn moves_the_key_out_of_the_keychain_once() {
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        let deleted = Cell::new(false);
        assert_eq!(kf.get_or_move(|| LegacyRead::Found(format!(" {KEY}\n")), || deleted.set(true)).as_deref(), Some(KEY));
        assert!(deleted.get(), "the Keychain copy is deleted after the file is written");
        assert_eq!(std::fs::read_to_string(t.0.join(KEY_FILE)).unwrap(), KEY);
        assert!(kf.moved());
        assert!(!has_tmp_files(&t.0));
        // Every later read (this launch or the next) comes from the file.
        assert_eq!(kf.get_or_move(never, || panic!("no second delete")).as_deref(), Some(KEY));
    }

    #[test]
    fn no_key_in_the_keychain_is_checked_only_once() {
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        assert_eq!(kf.get_or_move(|| LegacyRead::Missing, || panic!("nothing to delete")), None);
        assert!(kf.moved());
        assert_eq!(kf.get_or_move(never, || {}), None);
    }

    #[test]
    fn a_denied_keychain_prompt_is_retried_next_time_and_nothing_is_deleted() {
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        assert_eq!(kf.get_or_move(|| LegacyRead::Failed, || panic!("must not delete")), None);
        assert!(!kf.moved(), "the key isn't lost: the Keychain is asked again next launch");
        assert_eq!(kf.get_or_move(|| LegacyRead::Found(KEY.into()), || {}).as_deref(), Some(KEY));
    }

    #[test]
    fn failed_file_write_keeps_the_keychain_copy_and_still_returns_the_key() {
        let t = TempDir::new();
        // A directory at the target path blocks rename on both Windows and Unix.
        std::fs::create_dir(t.0.join(KEY_FILE)).unwrap();
        let kf = KeyFile::new(&t.0);
        let deleted = Cell::new(false);
        assert_eq!(kf.get_or_move(|| LegacyRead::Found(KEY.into()), || deleted.set(true)).as_deref(), Some(KEY));
        assert!(!deleted.get(), "must not delete the Keychain copy when the file write fails");
        assert!(!kf.moved(), "no marker so the move retries next launch");
        assert!(!has_tmp_files(&t.0), "temp file cleaned up on failure");
    }

    #[test]
    fn a_key_saved_before_the_move_wins_and_the_keychain_is_never_read() {
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        kf.write(KEY).unwrap();
        assert_eq!(kf.get_or_move(never, || {}).as_deref(), Some(KEY));
    }

    #[test]
    fn removing_the_key_never_brings_back_the_keychain_copy() {
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        kf.remove().unwrap(); // nothing saved yet: still fine
        assert_eq!(kf.get_or_move(never, || {}), None, "an old Keychain key stays gone after Remove");
        kf.write(KEY).unwrap();
        kf.remove().unwrap();
        assert!(!t.0.join(KEY_FILE).exists());
        assert_eq!(kf.get_or_move(never, || {}), None);
    }

    #[test]
    fn replacing_the_key_overwrites_it_and_an_empty_file_is_no_key() {
        let t = TempDir::new();
        let dir = t.0.join("not-yet-created");
        let kf = KeyFile::new(&dir);
        kf.write(KEY).unwrap();
        kf.write("sk-ant-api03-second-key-0123456789abcdefghij").unwrap();
        assert_eq!(kf.read().as_deref(), Some("sk-ant-api03-second-key-0123456789abcdefghij"));
        assert!(!has_tmp_files(&dir));
        std::fs::write(dir.join(KEY_FILE), "  \n").unwrap();
        assert_eq!(kf.read(), None);
    }

    #[test]
    fn an_empty_keychain_entry_counts_as_no_key() {
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        assert_eq!(kf.get_or_move(|| LegacyRead::Found("  ".into()), || panic!("nothing to delete")), None);
        assert!(kf.moved());
    }

    #[cfg(unix)]
    #[test]
    fn only_the_owner_can_read_the_key_file() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempDir::new();
        let kf = KeyFile::new(&t.0);
        kf.write(KEY).unwrap();
        let mode = std::fs::metadata(t.0.join(KEY_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
        // Rewriting keeps it private.
        kf.write(KEY).unwrap();
        let mode = std::fs::metadata(t.0.join(KEY_FILE)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
    }
}
