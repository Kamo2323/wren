//! The pinned speech runtime (`sidecar/runtime-manifest.json`, compiled in): which uv build to fetch,
//! which Python, which lock file folder, and every model file with its size and SHA-256.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

const MANIFEST_JSON: &str = include_str!("../../../sidecar/runtime-manifest.json");

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub python: String,
    pub uv: UvPins,
    pub platforms: BTreeMap<String, PlatformPins>,
    pub models: BTreeMap<String, ModelPins>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UvPins {
    pub version: String,
    #[serde(flatten)]
    pub builds: BTreeMap<String, UvBuild>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UvBuild {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// File name of the uv executable inside the archive (found wherever the archive puts it).
    pub exe: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformPins {
    /// Folder under the sidecar dir holding this platform's `pyproject.toml` + `uv.lock` ("" = the sidecar dir).
    pub project: String,
    /// Modules the finished venv must import (proves the packages really installed).
    pub verify_imports: Vec<String>,
    /// Estimates, for the size shown before setup starts.
    pub python_download_bytes: u64,
    pub packages_download_bytes: u64,
    pub runtime_disk_bytes: u64,
    /// Model folders this platform needs: the main model first, then the processor fallback.
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelPins {
    pub repo: String,
    pub revision: String,
    /// At least one of these must exist for the folder to count as a model.
    pub required: Vec<String>,
    pub files: Vec<ModelFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelFile {
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

impl ModelPins {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Download URL of one file at the pinned revision (`base` is `https://huggingface.co` outside tests).
    pub fn file_url(&self, base: &str, file: &str) -> String {
        format!("{}/{}/resolve/{}/{}", base.trim_end_matches('/'), self.repo, self.revision, file)
    }

    /// A folder counts as this model when a required file is there (the sidecar's own test).
    pub fn present_in(&self, dir: &Path) -> bool {
        self.required.iter().any(|f| dir.join(f).is_file())
    }
}

/// The compiled-in manifest. It is checked by tests, so a parse failure here is a build mistake.
pub fn manifest() -> &'static Manifest {
    static M: OnceLock<Manifest> = OnceLock::new();
    M.get_or_init(|| serde_json::from_str(MANIFEST_JSON).expect("sidecar/runtime-manifest.json is valid"))
}

/// Key of this build's platform in the manifest, or `None` where in-app setup is not offered.
pub fn platform_key() -> Option<&'static str> {
    if cfg!(all(windows, target_arch = "x86_64")) {
        Some("windows-x86_64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("macos-aarch64")
    } else {
        None
    }
}

/// This platform's pins (`None` = unsupported here).
pub fn platform() -> Option<(&'static PlatformPins, &'static UvBuild)> {
    let key = platform_key()?;
    let m = manifest();
    Some((m.platforms.get(key)?, m.uv.builds.get(key)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_hex64(s: &str) -> bool {
        s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    }

    #[test]
    fn manifest_is_complete_and_consistent() {
        let m = manifest();
        assert_eq!(m.version, 1);
        assert_eq!(m.python, "3.12");
        for key in ["windows-x86_64", "macos-aarch64"] {
            let p = m.platforms.get(key).unwrap_or_else(|| panic!("platform {key}"));
            let uv = m.uv.builds.get(key).unwrap_or_else(|| panic!("uv build {key}"));
            assert!(uv.url.starts_with("https://github.com/astral-sh/uv/releases/download/"), "{}", uv.url);
            assert!(uv.url.contains(&m.uv.version), "uv url pins the version: {}", uv.url);
            assert!(is_hex64(&uv.sha256) && uv.size > 1_000_000);
            assert!(!p.verify_imports.is_empty() && p.models.len() == 2);
            for name in &p.models {
                let model = m.models.get(name).unwrap_or_else(|| panic!("model {name}"));
                assert_eq!(model.revision.len(), 40, "{name} pins a full commit");
                assert!(model.required.iter().all(|r| model.files.iter().any(|f| &f.name == r)), "{name} required files are downloaded");
                for f in &model.files {
                    assert!(is_hex64(&f.sha256) && f.size > 0, "{name}/{}", f.name);
                    assert!(!f.name.contains('/') && !f.name.contains('\\') && !f.name.starts_with('.'), "flat file names only");
                }
            }
        }
    }

    #[test]
    fn this_platform_has_pins_where_setup_is_offered() {
        if cfg!(any(all(windows, target_arch = "x86_64"), all(target_os = "macos", target_arch = "aarch64"))) {
            assert!(platform().is_some());
        }
    }

    #[test]
    fn default_engine_models_are_pinned_for_this_platform() {
        let Some((p, _)) = platform() else { return };
        let d = crate::settings::EngineSettings::default();
        assert_eq!(p.models[0], d.model, "the main model is the default engine model");
        assert_eq!(p.models[1], d.cpu_fallback_model, "the second model is the processor fallback");
    }

    #[test]
    fn file_urls_use_the_pinned_revision() {
        let m = &manifest().models["small.en"];
        assert_eq!(
            m.file_url("https://huggingface.co/", "model.bin"),
            "https://huggingface.co/Systran/faster-whisper-small.en/resolve/d1d751a5f8271d482d14ca55d9e2deeebbae577f/model.bin"
        );
    }
}
