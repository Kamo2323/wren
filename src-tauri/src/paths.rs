//! Where Wren keeps its files (spec §12).
//!
//! Settings live under `%APPDATA%\Wren`; everything bulky (database, audio, models, the Python
//! runtime, logs) lives under `%LOCALAPPDATA%\Wren`. Setting `WREN_HOME` puts both roots under one
//! folder, which tests use so they never touch the real data.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    /// Roaming root (settings).
    pub roaming: PathBuf,
    /// Local root (db, audio, models, runtime, logs).
    pub local: PathBuf,
}

impl Paths {
    /// Resolve the real locations (or `WREN_HOME` if set).
    pub fn resolve() -> Self {
        if let Ok(home) = std::env::var("WREN_HOME") {
            return Self::under(Path::new(&home));
        }
        let roaming = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Wren");
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("Wren");
        Self { roaming, local }
    }

    /// Both roots under one folder (tests, portable runs).
    pub fn under(root: &Path) -> Self {
        Self {
            roaming: root.join("roaming"),
            local: root.join("local"),
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.roaming.join("settings.json")
    }
    pub fn db_dir(&self) -> PathBuf {
        self.local.join("db")
    }
    pub fn db_file(&self) -> PathBuf {
        self.db_dir().join("wren.db")
    }
    pub fn audio_dir(&self) -> PathBuf {
        self.local.join("audio")
    }
    pub fn models_dir(&self) -> PathBuf {
        self.local.join("models")
    }
    pub fn runtime_dir(&self) -> PathBuf {
        self.local.join("runtime")
    }
    /// Python interpreter inside the sidecar venv.
    pub fn runtime_python(&self) -> PathBuf {
        self.runtime_dir().join(".venv").join("Scripts").join("python.exe")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.local.join("logs")
    }

    /// Create every directory Wren writes to.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for dir in [
            self.roaming.clone(),
            self.db_dir(),
            self.audio_dir(),
            self.models_dir(),
            self.runtime_dir(),
            self.logs_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_keeps_everything_inside_the_root() {
        let p = Paths::under(Path::new("C:/tmp/wren-test"));
        assert!(p.settings_file().starts_with("C:/tmp/wren-test"));
        assert!(p.db_file().starts_with("C:/tmp/wren-test"));
        assert!(p.runtime_python().ends_with("python.exe"));
    }
}
