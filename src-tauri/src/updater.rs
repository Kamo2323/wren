//! In-app updater backed by GitHub Releases.
//!
//! Checks `https://github.com/<owner>/<repo>/releases/latest/download/latest.json` first (no auth,
//! no API rate limit — works automatically once the repo is public). If GitHub returns 404 because
//! the repository is private, it falls back to the GitHub REST API using a token from
//! `WREN_GITHUB_TOKEN`, `GITHUB_TOKEN`, or `gh auth token` (run silently with no console window).
//!
//! Every downloaded installer is verified against [`UPDATE_PUBLIC_KEY`] (Ed25519 minisign, with the
//! release version bound inside the signed trusted comment) before anything is executed. Installing
//! spawns a small detached PowerShell helper that waits for this `wren.exe` process to shut down
//! cleanly (unhooking the keyboard and stopping the Python sidecar), runs the NSIS installer with
//! `/S /UPDATE /R`, and cleans up afterwards.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use futures_util::StreamExt;
use parking_lot::{Mutex, RwLock};
use reqwest::header::{ACCEPT, AUTHORIZATION, LOCATION, USER_AGENT};
use semver::Version;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::constants::{
    UPDATE_CHECK_INTERVAL_MS, UPDATE_CHECK_TIMEOUT_MS, UPDATE_DOWNLOAD_TIMEOUT_MS, UPDATE_EXIT_DELAY_MS,
    UPDATE_FIRST_CHECK_DELAY_MS, UPDATE_GH_TOKEN_TIMEOUT_MS, UPDATE_PROGRESS_EMIT_MS, UPDATE_PUBLIC_KEY,
    UPDATE_REPO_NAME, UPDATE_REPO_OWNER, UPDATE_WAIT_PARENT_SECS,
};

/// Payload: [`UpdateStatus`], emitted whenever update state or download progress changes.
pub const EVENT_UPDATE_STATUS: &str = "app://update-status";
/// No payload: user clicked the tray's update item; front end navigates to Settings → Updates.
pub const EVENT_OPEN_UPDATES: &str = "tray://open-updates";

#[cfg(windows)]
const PLATFORM_KEY: &str = "windows-x86_64";
#[cfg(windows)]
const PLATFORM_KEY_ALT: &str = "windows-x86_64-nsis";
#[cfg(windows)]
const ASSET_SUFFIX: &str = "_x64-setup.exe";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PLATFORM_KEY: &str = "darwin-aarch64";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PLATFORM_KEY_ALT: &str = "darwin-aarch64-app";
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ASSET_SUFFIX: &str = "_aarch64.app.tar.gz";

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const PLATFORM_KEY: &str = "darwin-x86_64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const PLATFORM_KEY_ALT: &str = "darwin-x86_64-app";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const ASSET_SUFFIX: &str = "_x64.app.tar.gz";

/// Front-end update state (contract with `src/lib/types.ts` `UpdateStatus`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum UpdateStatus {
    Idle,
    Checking,
    #[serde(rename_all = "camelCase")]
    UpToDate {
        checked_at: String,
    },
    #[serde(rename_all = "camelCase")]
    Available {
        version: String,
        notes: Option<String>,
        pub_date: Option<String>,
        release_url: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Downloading {
        version: String,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        percent: Option<u8>,
    },
    #[serde(rename_all = "camelCase")]
    Installing {
        version: String,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
        version: Option<String>,
    },
}

/// Standard Tauri updater manifest (`latest.json`).
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub pub_date: Option<String>,
    #[serde(default)]
    pub platforms: HashMap<String, PlatformArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformArtifact {
    pub url: String,
    pub signature: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    url: String,
}

#[derive(Debug, Clone)]
enum DownloadSource {
    /// Public URL (no GitHub auth header needed).
    DirectUrl(String),
    /// Private-repo GitHub REST API asset URL (`Accept: application/octet-stream` + Bearer token).
    GitHubAssetApi { asset_url: String },
}

#[derive(Debug, Clone)]
struct PendingUpdate {
    version: String,
    notes: Option<String>,
    pub_date: Option<String>,
    release_url: Option<String>,
    signature_b64: String,
    source: DownloadSource,
}

#[derive(Debug, Clone)]
pub struct UpdaterConfig {
    pub public_latest_url: String,
    pub api_latest_release_url: String,
    pub public_key_b64: String,
    pub current_version: String,
    pub allow_insecure_localhost: bool,
}

impl UpdaterConfig {
    pub fn production(current_version: String) -> Self {
        Self {
            public_latest_url: format!(
                "https://github.com/{UPDATE_REPO_OWNER}/{UPDATE_REPO_NAME}/releases/latest/download/latest.json"
            ),
            api_latest_release_url: format!(
                "https://api.github.com/repos/{UPDATE_REPO_OWNER}/{UPDATE_REPO_NAME}/releases/latest"
            ),
            public_key_b64: UPDATE_PUBLIC_KEY.to_string(),
            current_version,
            allow_insecure_localhost: false,
        }
    }
}

/// Managed updater service shared across Tauri commands, tray events and the background checker.
pub struct UpdaterService {
    config: UpdaterConfig,
    updates_dir: PathBuf,
    logs_dir: PathBuf,
    status: RwLock<UpdateStatus>,
    pending: Mutex<Option<PendingUpdate>>,
    busy: AtomicBool,
}

struct BusyGuard<'a>(&'a AtomicBool);
impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl UpdaterService {
    pub fn new(current_version: String, updates_dir: PathBuf, logs_dir: PathBuf) -> Self {
        Self {
            config: UpdaterConfig::production(current_version),
            updates_dir,
            logs_dir,
            status: RwLock::new(UpdateStatus::Idle),
            pending: Mutex::new(None),
            busy: AtomicBool::new(false),
        }
    }

    pub fn current_status(&self) -> UpdateStatus {
        self.status.read().clone()
    }

    fn set_status(&self, app: &AppHandle, next: UpdateStatus) {
        *self.status.write() = next.clone();
        crate::tray::set_update_status(&next);
        let _ = app.emit(EVENT_UPDATE_STATUS, &next);
    }

    /// Starts the silent background checker (15 s after launch, then hourly).
    pub fn start_background(app: AppHandle) {
        if std::env::var_os("WREN_DISABLE_UPDATER").is_some_and(|v| !v.is_empty() && v != "0")
            || std::env::var_os("WREN_HOME").is_some_and(|v| !v.is_empty())
        {
            tracing::info!("background update checker disabled for test environment");
            return;
        }
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(UPDATE_FIRST_CHECK_DELAY_MS)).await;
            loop {
                {
                    let svc = app.state::<UpdaterService>();
                    let _ = svc.check(&app, false).await;
                }
                tokio::time::sleep(Duration::from_millis(UPDATE_CHECK_INTERVAL_MS)).await;
            }
        });
    }

    /// Checks GitHub Releases for a newer version.
    ///
    /// Background checks (`manual == false`) stay quiet on offline/auth errors so Wren never nags;
    /// manual checks from Settings or the tray report clear status and error messages.
    pub async fn check(&self, app: &AppHandle, manual: bool) -> UpdateStatus {
        if self.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return self.current_status();
        }
        let _guard = BusyGuard(&self.busy);

        if matches!(
            *self.status.read(),
            UpdateStatus::Downloading { .. } | UpdateStatus::Installing { .. }
        ) {
            return self.current_status();
        }

        let previous = self.current_status();
        if manual {
            self.set_status(app, UpdateStatus::Checking);
        }

        let config = self.config.clone();
        let outcome = check_remote(&config, resolve_github_token).await;

        match outcome {
            Ok(CheckResult::Available(pending)) => {
                tracing::info!(
                    current = %self.config.current_version,
                    available = %pending.version,
                    "Wren update available"
                );
                let next = UpdateStatus::Available {
                    version: pending.version.clone(),
                    notes: pending.notes.clone(),
                    pub_date: pending.pub_date.clone(),
                    release_url: pending.release_url.clone(),
                };
                *self.pending.lock() = Some(pending);
                self.set_status(app, next.clone());
                next
            }
            Ok(CheckResult::UpToDate) => {
                tracing::info!(current = %self.config.current_version, "Wren is up to date");
                *self.pending.lock() = None;
                let next = UpdateStatus::UpToDate {
                    checked_at: chrono::Utc::now().to_rfc3339(),
                };
                if manual || matches!(previous, UpdateStatus::Available { .. } | UpdateStatus::Error { .. }) {
                    self.set_status(app, next.clone());
                } else {
                    *self.status.write() = next.clone();
                }
                next
            }
            Err(err) => {
                tracing::info!(error = %err, manual, "update check did not finish");
                if manual {
                    let next = UpdateStatus::Error {
                        message: err,
                        version: self.pending.lock().as_ref().map(|p| p.version.clone()),
                    };
                    self.set_status(app, next.clone());
                    next
                } else {
                    // Keep the existing status (e.g. Idle or a previously discovered Available) silently.
                    previous
                }
            }
        }
    }

    /// Downloads the pending update, verifies its Ed25519 minisign signature, spawns the detached
    /// silent NSIS installer helper, and exits Wren cleanly so the installer can replace it and
    /// relaunch the updated app.
    pub async fn install(&self, app: &AppHandle) -> Result<(), String> {
        if matches!(
            *self.status.read(),
            UpdateStatus::Downloading { .. } | UpdateStatus::Installing { .. }
        ) {
            return Ok(());
        }

        if self.pending.lock().is_none() {
            let status = self.check(app, true).await;
            if !matches!(status, UpdateStatus::Available { .. }) {
                return match status {
                    UpdateStatus::UpToDate { .. } => Ok(()),
                    UpdateStatus::Error { message, .. } => Err(message),
                    _ => Err("No update is ready to install.".into()),
                };
            }
        }

        if self.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return Ok(());
        }
        let _guard = BusyGuard(&self.busy);

        let Some(pending) = self.pending.lock().clone() else {
            return Err("No update is ready to install.".into());
        };

        let version = pending.version.clone();
        self.set_status(
            app,
            UpdateStatus::Downloading {
                version: version.clone(),
                downloaded_bytes: 0,
                total_bytes: None,
                percent: Some(0),
            },
        );

        let installer_path = match self.download_and_verify(app, &pending).await {
            Ok(path) => path,
            Err(message) => {
                tracing::warn!(version = %version, error = %message, "update download or verification failed");
                self.set_status(
                    app,
                    UpdateStatus::Error {
                        message: message.clone(),
                        version: Some(version),
                    },
                );
                return Err(message);
            }
        };

        self.set_status(
            app,
            UpdateStatus::Installing {
                version: version.clone(),
            },
        );

        let fallback_exe = std::env::current_exe().ok();
        let log_path = self.logs_dir.join("updater.log");
        if let Err(message) = spawn_install_helper(
            &self.updates_dir,
            std::process::id(),
            &installer_path,
            fallback_exe.as_deref(),
            &log_path,
        ) {
            tracing::error!(version = %version, error = %message, "cannot launch update installer helper");
            self.set_status(
                app,
                UpdateStatus::Error {
                    message: message.clone(),
                    version: Some(version),
                },
            );
            return Err(message);
        }

        tracing::info!(version = %version, installer = %installer_path.display(), "update verified; exiting so the installer can finish");
        let exit_handle = app.clone();
        std::thread::Builder::new()
            .name("wren-update-exit".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(UPDATE_EXIT_DELAY_MS));
                exit_handle.exit(0);
            })
            .map_err(|e| format!("Couldn't restart Wren for the update: {e}"))?;

        Ok(())
    }

    async fn download_and_verify(&self, app: &AppHandle, pending: &PendingUpdate) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.updates_dir)
            .map_err(|e| format!("Couldn't prepare the updates folder: {e}"))?;
        cleanup_old_updates(&self.updates_dir);

        let safe_ver = sanitize_version_for_filename(&pending.version);
        let final_path = self.updates_dir.join(format!("Wren_{safe_ver}{ASSET_SUFFIX}"));
        let part_path = self.updates_dir.join(format!("Wren_{safe_ver}{ASSET_SUFFIX}.part"));
        let _ = std::fs::remove_file(&part_path);

        let token = match &pending.source {
            DownloadSource::DirectUrl(_) => None,
            DownloadSource::GitHubAssetApi { .. } => resolve_github_token(),
        };

        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut last_pct: Option<u8> = None;
        let version = pending.version.clone();

        let bytes = download_installer_to_file(
            &pending.source,
            token.as_deref(),
            &self.config.current_version,
            self.config.allow_insecure_localhost,
            &part_path,
            |downloaded, total| {
                let pct = total.and_then(|t| (t > 0).then(|| ((downloaded.saturating_mul(100)) / t).min(100) as u8));
                let elapsed_ok = last_emit.elapsed() >= Duration::from_millis(UPDATE_PROGRESS_EMIT_MS);
                let pct_changed = pct != last_pct && pct.is_some();
                if elapsed_ok || pct_changed || pct == Some(100) {
                    last_emit = Instant::now();
                    last_pct = pct;
                    self.set_status(
                        app,
                        UpdateStatus::Downloading {
                            version: version.clone(),
                            downloaded_bytes: downloaded,
                            total_bytes: total,
                            percent: pct,
                        },
                    );
                }
            },
        )
        .await
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&part_path);
        })?;

        verify_signature(&self.config.public_key_b64, &bytes, &pending.signature_b64, &pending.version).map_err(|e| {
            let _ = std::fs::remove_file(&part_path);
            tracing::error!(error = %e, "update signature verification failed");
            "The downloaded update couldn't be verified and was discarded.".to_string()
        })?;

        let _ = std::fs::remove_file(&final_path);
        std::fs::rename(&part_path, &final_path).map_err(|e| {
            let _ = std::fs::remove_file(&part_path);
            format!("Couldn't save the downloaded update: {e}")
        })?;

        Ok(final_path)
    }
}

// ---- Core check, download and verification logic (pure / testable) ----

#[derive(Debug)]
enum CheckResult {
    UpToDate,
    Available(PendingUpdate),
}

fn user_agent(version: &str) -> String {
    format!("Wren/{version}")
}

fn build_client(timeout: Duration, follow_redirects: bool) -> Result<reqwest::Client, String> {
    let mut b = reqwest::Client::builder().timeout(timeout);
    if !follow_redirects {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    b.build().map_err(|e| format!("Couldn't start the network client: {e}"))
}

fn validate_download_url(url: &str, allow_insecure_localhost: bool) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "The update address wasn't valid.".to_string())?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" if allow_insecure_localhost && matches!(parsed.host_str(), Some("127.0.0.1" | "localhost")) => Ok(()),
        _ => Err("Refusing to download an update over an insecure connection.".to_string()),
    }
}

/// Parses a version string (with or without a leading `v`) and returns whether `remote` is newer.
pub fn is_newer_version(current: &str, remote: &str) -> Result<bool, String> {
    let cur = Version::parse(current.trim().trim_start_matches(['v', 'V']))
        .map_err(|e| format!("Current version '{current}' isn't valid semver: {e}"))?;
    let rem = Version::parse(remote.trim().trim_start_matches(['v', 'V']))
        .map_err(|e| format!("Release version '{remote}' isn't valid semver: {e}"))?;
    Ok(rem > cur)
}

fn pick_platform_artifact(manifest: &UpdateManifest) -> Result<&PlatformArtifact, String> {
    let art = manifest
        .platforms
        .get(PLATFORM_KEY)
        .or_else(|| manifest.platforms.get(PLATFORM_KEY_ALT))
        .ok_or_else(|| {
            if cfg!(target_os = "macos") {
                "This release doesn't include a macOS build yet.".to_string()
            } else {
                "This release doesn't include a Windows installer.".to_string()
            }
        })?;
    if art.url.trim().is_empty() || art.signature.trim().is_empty() {
        return Err("This release is missing its installer or signature.".to_string());
    }
    Ok(art)
}

fn filename_from_url(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)))
        .filter(|s| !s.is_empty())
}

fn clean_notes(notes: Option<String>) -> Option<String> {
    notes.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn sanitize_version_for_filename(version: &str) -> String {
    let cleaned: String = version
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if cleaned.is_empty() { "update".into() } else { cleaned }
}

async fn check_remote<F>(config: &UpdaterConfig, token_provider: F) -> Result<CheckResult, String>
where
    F: FnOnce() -> Option<String>,
{
    let timeout = Duration::from_millis(UPDATE_CHECK_TIMEOUT_MS);
    let client = build_client(timeout, true)?;
    let ua = user_agent(&config.current_version);

    // 1. Try the public GitHub Releases URL first (no token, no API rate limit).
    let public_resp = client
        .get(&config.public_latest_url)
        .header(USER_AGENT, &ua)
        .header(ACCEPT, "application/json")
        .send()
        .await;

    match public_resp {
        Ok(resp) if resp.status().is_success() => {
            let text = resp
                .text()
                .await
                .map_err(|_| "Couldn't read the update details from GitHub.".to_string())?;
            let manifest: UpdateManifest = serde_json::from_str(&text)
                .map_err(|_| "GitHub returned unreadable update details.".to_string())?;
            return evaluate_manifest(config, manifest, None, None, None);
        }
        Ok(resp) if resp.status().as_u16() == 404 => {
            // Either the repository is private or no release has been published yet. Fall through
            // to the GitHub REST API with a local token if available.
        }
        Ok(resp) => {
            return Err(format!("GitHub answered with HTTP {}. Try again shortly.", resp.status().as_u16()));
        }
        Err(_) => {
            return Err("Couldn't reach GitHub. Check your internet connection and try again.".to_string());
        }
    }

    // 2. Private-repo fallback via GitHub REST API + token (env var or `gh auth token`).
    let token = token_provider();
    let Some(token) = token else {
        return Err(
            "No public release was found. While the repo is private, sign in with GitHub CLI (gh auth login) so Wren can check for updates."
                .to_string(),
        );
    };

    let api_resp = client
        .get(&config.api_latest_release_url)
        .header(USER_AGENT, &ua)
        .header(ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|_| "Couldn't reach GitHub. Check your internet connection and try again.".to_string())?;

    let status = api_resp.status().as_u16();
    if status == 404 {
        // Authenticated against the private repo, and no releases exist yet.
        return Ok(CheckResult::UpToDate);
    }
    if status == 401 || status == 403 {
        return Err("GitHub refused the update check. Run gh auth login and try again.".to_string());
    }
    if !api_resp.status().is_success() {
        return Err(format!("GitHub answered with HTTP {status}. Try again shortly."));
    }

    let release_bytes = api_resp
        .bytes()
        .await
        .map_err(|_| "GitHub returned unreadable release details.".to_string())?;
    let release: GitHubRelease = serde_json::from_slice(&release_bytes)
        .map_err(|_| "GitHub returned unreadable release details.".to_string())?;

    let latest_asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("latest.json"))
        .ok_or_else(|| {
            format!(
                "Release {} doesn't have a latest.json update manifest yet.",
                release.tag_name
            )
        })?;

    let manifest_bytes = download_github_asset_bytes(
        &latest_asset.url,
        Some(&token),
        &config.current_version,
        config.allow_insecure_localhost,
        timeout,
    )
    .await?;

    let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| "The release's latest.json file wasn't valid JSON.".to_string())?;

    evaluate_manifest(config, manifest, Some(&release), release.html_url.clone(), release.body.clone())
}

fn evaluate_manifest(
    config: &UpdaterConfig,
    manifest: UpdateManifest,
    release: Option<&GitHubRelease>,
    fallback_release_url: Option<String>,
    fallback_notes: Option<String>,
) -> Result<CheckResult, String> {
    let clean_ver = manifest.version.trim().trim_start_matches(['v', 'V']).to_string();
    if !is_newer_version(&config.current_version, &clean_ver)? {
        return Ok(CheckResult::UpToDate);
    }

    let artifact = pick_platform_artifact(&manifest)?;
    validate_download_url(&artifact.url, config.allow_insecure_localhost)?;
    let signature_b64 = artifact.signature.trim().to_string();

    let source = if let Some(rel) = release {
        let wanted = filename_from_url(&artifact.url);
        let asset = wanted
            .as_deref()
            .and_then(|name| rel.assets.iter().find(|a| a.name == name))
            .or_else(|| rel.assets.iter().find(|a| a.name.ends_with(ASSET_SUFFIX)))
            .ok_or_else(|| {
                if cfg!(target_os = "macos") {
                    "This release doesn't have the macOS app archive attached.".to_string()
                } else {
                    "This release doesn't have the Windows installer attached.".to_string()
                }
            })?;
        validate_download_url(&asset.url, config.allow_insecure_localhost)?;
        DownloadSource::GitHubAssetApi {
            asset_url: asset.url.clone(),
        }
    } else {
        DownloadSource::DirectUrl(artifact.url.clone())
    };

    let release_url = fallback_release_url.or_else(|| {
        Some(format!(
            "https://github.com/{UPDATE_REPO_OWNER}/{UPDATE_REPO_NAME}/releases/tag/v{clean_ver}"
        ))
    });

    Ok(CheckResult::Available(PendingUpdate {
        version: clean_ver,
        notes: clean_notes(manifest.notes).or_else(|| clean_notes(fallback_notes)),
        pub_date: manifest.pub_date.or_else(|| release.and_then(|r| r.published_at.clone())),
        release_url,
        signature_b64,
        source,
    }))
}

/// Fetches a GitHub Release asset via the API without forwarding `Authorization` to the redirected
/// blob host (`objects.githubusercontent.com`).
async fn open_github_asset_response(
    asset_url: &str,
    token: Option<&str>,
    current_version: &str,
    allow_insecure_localhost: bool,
    timeout: Duration,
) -> Result<reqwest::Response, String> {
    validate_download_url(asset_url, allow_insecure_localhost)?;
    let ua = user_agent(current_version);
    let no_redirect = build_client(timeout, false)?;

    let mut req = no_redirect
        .get(asset_url)
        .header(USER_AGENT, &ua)
        .header(ACCEPT, "application/octet-stream")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(t) = token {
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }

    let resp = req
        .send()
        .await
        .map_err(|_| "Couldn't reach GitHub to download the update.".to_string())?;

    if resp.status().is_redirection() {
        let loc = resp
            .headers()
            .get(LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| "GitHub sent an incomplete download redirect.".to_string())?;
        validate_download_url(loc, allow_insecure_localhost)?;
        let clean_client = build_client(timeout, true)?;
        let blob_resp = clean_client
            .get(loc)
            .header(USER_AGENT, &ua)
            .send()
            .await
            .map_err(|_| "The download connection was interrupted. Try again.".to_string())?;
        if !blob_resp.status().is_success() {
            return Err(format!("Download failed with HTTP {}. Try again.", blob_resp.status().as_u16()));
        }
        return Ok(blob_resp);
    }

    let code = resp.status().as_u16();
    if code == 401 || code == 403 {
        return Err("GitHub refused the update download. Run gh auth login and try again.".to_string());
    }
    if !resp.status().is_success() {
        return Err(format!("Download failed with HTTP {code}. Try again."));
    }
    Ok(resp)
}

async fn download_github_asset_bytes(
    asset_url: &str,
    token: Option<&str>,
    current_version: &str,
    allow_insecure_localhost: bool,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let resp = open_github_asset_response(asset_url, token, current_version, allow_insecure_localhost, timeout).await?;
    resp.bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|_| "Couldn't finish reading the update metadata from GitHub.".to_string())
}

async fn download_installer_to_file<F>(
    source: &DownloadSource,
    token: Option<&str>,
    current_version: &str,
    allow_insecure_localhost: bool,
    dest: &Path,
    mut on_progress: F,
) -> Result<Vec<u8>, String>
where
    F: FnMut(u64, Option<u64>),
{
    let timeout = Duration::from_millis(UPDATE_DOWNLOAD_TIMEOUT_MS);
    let resp = match source {
        DownloadSource::DirectUrl(url) => {
            validate_download_url(url, allow_insecure_localhost)?;
            let client = build_client(timeout, true)?;
            let r = client
                .get(url)
                .header(USER_AGENT, user_agent(current_version))
                .send()
                .await
                .map_err(|_| "Couldn't reach GitHub to download the update.".to_string())?;
            if !r.status().is_success() {
                return Err(format!("Download failed with HTTP {}. Try again.", r.status().as_u16()));
            }
            r
        }
        DownloadSource::GitHubAssetApi { asset_url } => {
            open_github_asset_response(asset_url, token, current_version, allow_insecure_localhost, timeout).await?
        }
    };

    let total = resp.content_length();
    let mut file = std::fs::File::create(dest).map_err(|e| format!("Couldn't create the update file: {e}"))?;
    let mut buf = Vec::with_capacity(total.unwrap_or(0).min(32 * 1024 * 1024) as usize);
    let mut downloaded: u64 = 0;
    on_progress(0, total);

    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "The download was interrupted before it finished. Try again.".to_string())?;
        file.write_all(&chunk)
            .map_err(|e| format!("Couldn't write the update file: {e}"))?;
        buf.extend_from_slice(&chunk);
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        on_progress(downloaded, total);
    }

    file.sync_all()
        .map_err(|e| format!("Couldn't finish saving the update file: {e}"))?;

    if buf.is_empty() {
        return Err("The downloaded update file was empty.".to_string());
    }
    Ok(buf)
}

/// Verifies `payload` against the base64-encoded minisign public key and signature produced by
/// `tauri signer sign --app-version <expected_version>`.
pub fn verify_signature(
    public_key_b64: &str,
    payload: &[u8],
    signature_b64: &str,
    expected_version: &str,
) -> Result<(), String> {
    let b64 = base64::engine::general_purpose::STANDARD;

    let pk_raw = b64
        .decode(public_key_b64.trim())
        .map_err(|e| format!("invalid base64 public key: {e}"))?;
    let pk_text = String::from_utf8(pk_raw).map_err(|e| format!("public key is not utf-8: {e}"))?;
    let pk_line = pk_text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .ok_or_else(|| "public key box has no key line".to_string())?;
    let pk = minisign_verify::PublicKey::from_base64(pk_line)
        .map_err(|e| format!("invalid minisign public key: {e}"))?;

    let sig_raw = b64
        .decode(signature_b64.trim())
        .map_err(|e| format!("invalid base64 signature: {e}"))?;
    let sig_text = String::from_utf8(sig_raw).map_err(|e| format!("signature is not utf-8: {e}"))?;
    let sig = minisign_verify::Signature::decode(&sig_text)
        .map_err(|e| format!("invalid minisign signature: {e}"))?;

    pk.verify(payload, &sig, false)
        .map_err(|e| format!("signature verification failed: {e}"))?;

    // Enforce version binding embedded by `tauri signer sign --app-version <ver>` so a tampered
    // manifest cannot pair a newer version string with an older signed installer.
    let expected_ver = expected_version.trim().trim_start_matches(['v', 'V']);
    let signed_version = sig
        .trusted_comment()
        .split('\t')
        .find_map(|part| part.strip_prefix("version:"))
        .map(str::trim);
    match signed_version {
        Some(v) if v.trim_start_matches(['v', 'V']) == expected_ver => Ok(()),
        Some(v) => Err(format!("signature is bound to version {v}, expected {expected_ver}")),
        None => Err("signature is missing its signed version tag".to_string()),
    }
}

/// Resolves a GitHub token for private-repo access: env var first, then `gh auth token`.
pub fn resolve_github_token() -> Option<String> {
    for var in ["WREN_GITHUB_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    gh_auth_token()
}

fn gh_exe() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(paths) = std::env::var_os("PATH")
            && let Some(found) = std::env::split_paths(&paths).map(|d| d.join("gh.exe")).find(|p| p.exists())
        {
            return Some(found);
        }
        let mut candidates = vec![
            PathBuf::from(r"C:\Program Files\GitHub CLI\gh.exe"),
            PathBuf::from(r"C:\Program Files (x86)\GitHub CLI\gh.exe"),
        ];
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join("Programs").join("GitHub CLI").join("gh.exe"));
        }
        candidates.into_iter().find(|p| p.exists())
    }
    #[cfg(not(windows))]
    {
        if let Some(paths) = std::env::var_os("PATH")
            && let Some(found) = std::env::split_paths(&paths).map(|d| d.join("gh")).find(|p| p.is_file())
        {
            return Some(found);
        }
        ["/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }
}

fn gh_auth_token() -> Option<String> {
    use std::io::Read;

    let exe = gh_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;

    let deadline = Instant::now() + Duration::from_millis(UPDATE_GH_TOKEN_TIMEOUT_MS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                let trimmed = out.trim();
                return (!trimmed.is_empty()).then(|| trimmed.to_string());
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => return None,
        }
    }
}

fn cleanup_old_updates(updates_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(updates_dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// PowerShell script that waits for `wren.exe` to finish shutting down, runs the silent NSIS
/// installer with `/S /UPDATE /R`, logs the outcome, ensures Wren relaunches, and cleans up.
pub fn apply_script_body() -> String {
    format!(
        r#"param(
  [int]$ParentPid,
  [string]$InstallerPath,
  [string]$FallbackExe,
  [string]$LogPath
)
$ErrorActionPreference = 'SilentlyContinue'
function Write-Log([string]$msg) {{
  if ($LogPath) {{
    $dir = Split-Path -Parent $LogPath
    if ($dir) {{ New-Item -ItemType Directory -Force -Path $dir | Out-Null }}
    Add-Content -Path $LogPath -Value ("$(Get-Date -Format o) $msg")
  }}
}}
if ($ParentPid -gt 0) {{
  Write-Log "Waiting for Wren (pid $ParentPid) to exit..."
  try {{
    Wait-Process -Id $ParentPid -Timeout {UPDATE_WAIT_PARENT_SECS} -ErrorAction SilentlyContinue
  }} catch {{}}
  if (Get-Process -Id $ParentPid -ErrorAction SilentlyContinue) {{
    Write-Log "Wren (pid $ParentPid) did not exit in time; forcing stop before install"
    Stop-Process -Id $ParentPid -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 1000
  }}
}}
Start-Sleep -Milliseconds 500
Write-Log "Starting silent installer: $InstallerPath /S /UPDATE /R"
$code = -1
try {{
  $proc = Start-Process -FilePath $InstallerPath -ArgumentList '/S', '/UPDATE', '/R' -WindowStyle Hidden -PassThru
  if ($proc) {{
    $null = $proc.Handle
    $proc.WaitForExit()
    $code = [int]$proc.ExitCode
  }}
}} catch {{
  Write-Log "Failed to launch installer: $_"
}}
Write-Log "Installer finished with exit code $code"
Remove-Item -LiteralPath $InstallerPath -Force -ErrorAction SilentlyContinue
if ($code -eq 0) {{
  if ($FallbackExe -and (Test-Path -LiteralPath $FallbackExe)) {{
    $restarted = $false
    for ($i = 0; $i -lt 12; $i++) {{
      if (Get-Process -Name 'wren' -ErrorAction SilentlyContinue) {{
        $restarted = $true
        break
      }}
      Start-Sleep -Milliseconds 250
    }}
    if (-not $restarted) {{
      Write-Log "Installer succeeded; relaunching $FallbackExe"
      Start-Process -FilePath $FallbackExe
    }}
  }}
}} elseif ($FallbackExe -and (Test-Path -LiteralPath $FallbackExe)) {{
  Write-Log "Installer failed (code $code); relaunching $FallbackExe"
  Start-Process -FilePath $FallbackExe
}}
Remove-Item -LiteralPath $MyInvocation.MyCommand.Path -Force -ErrorAction SilentlyContinue
"#
    )
}

#[cfg(windows)]
fn powershell_exe() -> PathBuf {
    let sys = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    if sys.exists() { sys } else { PathBuf::from("powershell.exe") }
}

#[cfg(windows)]
fn spawn_install_helper(
    updates_dir: &Path,
    parent_pid: u32,
    installer_path: &Path,
    fallback_exe: Option<&Path>,
    log_path: &Path,
) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    // IMPORTANT: Do NOT combine `DETACHED_PROCESS` with `CREATE_NO_WINDOW`.
    // Per Win32 CreateProcess docs, `CREATE_NO_WINDOW` is ignored when `DETACHED_PROCESS` is set,
    // which starts Windows PowerShell 5.1 with no console host and causes `powershell.exe` to exit
    // immediately with status 0 without executing `-File`. `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP`
    // gives PowerShell its own hidden console host that survives parent process exit.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    let script_path = updates_dir.join("apply-update.ps1");
    std::fs::write(&script_path, apply_script_body())
        .map_err(|e| format!("Couldn't prepare the update helper: {e}"))?;

    let mut cmd = std::process::Command::new(powershell_exe());
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-WindowStyle",
        "Hidden",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ])
    .arg(&script_path)
    .arg("-ParentPid")
    .arg(parent_pid.to_string())
    .arg("-InstallerPath")
    .arg(installer_path)
    .arg("-FallbackExe")
    .arg(fallback_exe.map(|p| p.as_os_str()).unwrap_or_default())
    .arg("-LogPath")
    .arg(log_path)
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);

    cmd.spawn()
        .map_err(|e| format!("Couldn't start the update installer: {e}"))?;
    Ok(())
}

/// Resolves the enclosing `.app` bundle directory from an executable path
/// (`.../Wren.app/Contents/MacOS/wren` → `.../Wren.app`), falling back to `/Applications/Wren.app`.
#[cfg(target_os = "macos")]
pub fn resolve_macos_app_bundle(exe: Option<&Path>) -> PathBuf {
    if let Some(mut cur) = exe.map(Path::to_path_buf) {
        while let Some(parent) = cur.parent() {
            if cur.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("app")) {
                return cur;
            }
            cur = parent.to_path_buf();
        }
    }
    PathBuf::from("/Applications/Wren.app")
}

/// POSIX shell helper that waits for Wren to exit cleanly, extracts the verified `.app.tar.gz`,
/// atomically swaps the `.app` bundle (with rollback on error), clears quarantine attributes,
/// cleans up, and relaunches Wren (unless `NO_RELAUNCH=1` for tests).
#[cfg(target_os = "macos")]
pub fn macos_apply_script_body() -> String {
    format!(
        r#"#!/bin/sh
set -u
PARENT_PID="$1"
ARCHIVE_PATH="$2"
TARGET_APP="$3"
LOG_PATH="$4"
NO_RELAUNCH="${{5:-0}}"

log() {{
  if [ -n "$LOG_PATH" ]; then
    mkdir -p "$(dirname "$LOG_PATH")" 2>/dev/null || true
    printf '%s %s\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "$1" >> "$LOG_PATH"
  fi
}}

if [ "$PARENT_PID" -gt 0 ] 2>/dev/null; then
  log "Waiting for Wren (pid $PARENT_PID) to exit..."
  i=0
  max=$(( {UPDATE_WAIT_PARENT_SECS} * 4 ))
  while kill -0 "$PARENT_PID" 2>/dev/null; do
    if [ "$i" -ge "$max" ]; then
      log "Wren (pid $PARENT_PID) did not exit in time; forcing stop before install"
      kill -9 "$PARENT_PID" 2>/dev/null || true
      sleep 1
      break
    fi
    sleep 0.25
    i=$((i + 1))
  done
fi

sleep 0.3
log "Extracting update archive: $ARCHIVE_PATH -> $TARGET_APP"
TARGET_PARENT="$(dirname "$TARGET_APP")"
STAGE_DIR="$(mktemp -d "${{TARGET_PARENT}}/.wren-update.XXXXXX" 2>/dev/null || mktemp -d "${{TMPDIR:-/tmp}}/wren-update.XXXXXX")"
if ! tar -xzf "$ARCHIVE_PATH" -C "$STAGE_DIR" 2>>"$LOG_PATH"; then
  log "Archive extraction failed"
  rm -rf "$STAGE_DIR" "$ARCHIVE_PATH" "$0"
  [ "$NO_RELAUNCH" != "1" ] && open -n "$TARGET_APP" 2>/dev/null || true
  exit 1
fi

if [ -d "$STAGE_DIR/Wren.app" ]; then
  NEW_APP="$STAGE_DIR/Wren.app"
else
  NEW_APP="$(find "$STAGE_DIR" -maxdepth 1 -name "*.app" -type d | head -n 1)"
fi
if [ -z "$NEW_APP" ] || [ ! -d "$NEW_APP" ]; then
  log "No .app bundle found inside archive"
  rm -rf "$STAGE_DIR" "$ARCHIVE_PATH" "$0"
  [ "$NO_RELAUNCH" != "1" ] && open -n "$TARGET_APP" 2>/dev/null || true
  exit 1
fi

BACKUP_APP="${{TARGET_APP}}.wren-old"
rm -rf "$BACKUP_APP" 2>/dev/null || true
if [ -d "$TARGET_APP" ]; then
  if ! mv "$TARGET_APP" "$BACKUP_APP"; then
    log "Failed to move existing bundle aside"
    rm -rf "$STAGE_DIR" "$ARCHIVE_PATH" "$0"
    [ "$NO_RELAUNCH" != "1" ] && open -n "$TARGET_APP" 2>/dev/null || true
    exit 1
  fi
fi

if mv "$NEW_APP" "$TARGET_APP"; then
  /usr/bin/xattr -dr com.apple.quarantine "$TARGET_APP" 2>/dev/null || true
  if [ "$NO_RELAUNCH" != "1" ] && [ -x /usr/bin/codesign ] && [ -x /usr/bin/security ]; then
    SIGN_ID=""
    if [ -d "$BACKUP_APP" ]; then
      PREV_AUTH="$(/usr/bin/codesign -dvv "$BACKUP_APP" 2>&1 | /usr/bin/sed -n 's/^Authority=//p' | /usr/bin/head -n 1 || true)"
      if [ -n "$PREV_AUTH" ] && /usr/bin/security find-identity -v -p codesigning 2>/dev/null | /usr/bin/grep -Fq "\"$PREV_AUTH\""; then
        SIGN_ID="$PREV_AUTH"
      fi
    fi
    if [ -z "$SIGN_ID" ]; then
      SIGN_ID="$(/usr/bin/security find-identity -v -p codesigning 2>/dev/null | /usr/bin/grep -E '"(Developer ID Application|Apple Development):' | /usr/bin/head -n 1 | /usr/bin/sed -E 's/.*"([^"]+)".*/\1/' || true)"
    fi
    if [ -n "$SIGN_ID" ]; then
      log "Preserving local code-signing identity: $SIGN_ID"
      if ! /usr/bin/codesign --force --deep --sign "$SIGN_ID" "$TARGET_APP" 2>>"$LOG_PATH"; then
        log "Local re-sign failed; keeping archive signature"
      fi
    fi
  fi
  rm -rf "$BACKUP_APP" "$STAGE_DIR" "$ARCHIVE_PATH"
  log "Installer finished with exit code 0"
  if [ "$NO_RELAUNCH" != "1" ]; then
    log "Relaunching $TARGET_APP"
    /usr/bin/open -n "$TARGET_APP" 2>/dev/null || true
  fi
else
  log "Failed to swap new bundle into place; rolling back"
  rm -rf "$TARGET_APP" 2>/dev/null || true
  [ -d "$BACKUP_APP" ] && mv "$BACKUP_APP" "$TARGET_APP" 2>/dev/null || true
  rm -rf "$STAGE_DIR" "$ARCHIVE_PATH"
  [ "$NO_RELAUNCH" != "1" ] && open -n "$TARGET_APP" 2>/dev/null || true
fi

rm -f "$0" 2>/dev/null || true
"#
    )
}

#[cfg(target_os = "macos")]
fn spawn_install_helper(
    updates_dir: &Path,
    parent_pid: u32,
    installer_path: &Path,
    fallback_exe: Option<&Path>,
    log_path: &Path,
) -> Result<(), String> {
    spawn_macos_install_helper(
        updates_dir,
        parent_pid,
        installer_path,
        &resolve_macos_app_bundle(fallback_exe),
        log_path,
        false,
    )
}

#[cfg(target_os = "macos")]
fn check_macos_target_writable(target_app: &Path) -> Result<(), String> {
    let parent = target_app
        .parent()
        .ok_or_else(|| "Couldn't find the folder containing Wren.app.".to_string())?;
    let probe = parent.join(format!(".wren-update-probe-{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(format!(
            "Wren can't update itself inside {} (read-only or restricted folder). Move Wren.app into Applications and try again.",
            parent.display()
        )),
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos_install_helper(
    updates_dir: &Path,
    parent_pid: u32,
    archive_path: &Path,
    target_app: &Path,
    log_path: &Path,
    no_relaunch: bool,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt;

    check_macos_target_writable(target_app)?;

    let script_path = updates_dir.join("apply-update.sh");
    std::fs::write(&script_path, macos_apply_script_body())
        .map_err(|e| format!("Couldn't prepare the update helper: {e}"))?;
    let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700));

    let mut cmd = std::process::Command::new("/bin/sh");
    cmd.arg(&script_path)
        .arg(parent_pid.to_string())
        .arg(archive_path)
        .arg(target_app)
        .arg(log_path)
        .arg(if no_relaunch { "1" } else { "0" })
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);

    cmd.spawn()
        .map_err(|e| format!("Couldn't start the update installer: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex as StdMutex};

    // Real Tauri-generated test signature over `b"hello wren updater"` bound to `--app-version 0.1.2`
    // using UPDATE_PUBLIC_KEY.
    const SAMPLE_PAYLOAD: &[u8] = b"hello wren updater";
    const SAMPLE_SIG_0_1_2: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUWGVIVU4xVHYwRTdrNFRJRW1zaGJNNXVQVFNsWmd3R3gwalNTYjIvZ1cwaDdhelZleU44d3A3bDllWUx5QUdnSE1zWEg4NGFndjBGM1ZRamlWYWJYTldyZU9uK1o5SUF3PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1NDAxNTI4CWZpbGU6c2FtcGxlLmJpbgl2ZXJzaW9uOjAuMS4yCncxamNvS2hkS1lxbS9kZjNZV2R6OERVT3MrUkFBMlh5N0JmMCt0dGNzUHF0UWpVYWNEWVV1dVQxbTBCZHNicTBtZzUxSzk3VjNmanh2OWJkRmY5b0FnPT0K";

    #[test]
    fn semver_comparison_handles_v_prefix_and_ordering() {
        assert!(is_newer_version("0.1.0", "0.1.1").unwrap());
        assert!(is_newer_version("0.1.0", "v0.1.12").unwrap());
        assert!(is_newer_version("0.1.99", "0.2.0").unwrap());
        assert!(!is_newer_version("0.1.2", "0.1.2").unwrap());
        assert!(!is_newer_version("0.1.5", "0.1.4").unwrap());
        assert!(is_newer_version("0.1.0", "not-a-version").is_err());
    }

    #[test]
    fn minisign_verification_accepts_valid_and_rejects_tampering_or_wrong_version() {
        verify_signature(UPDATE_PUBLIC_KEY, SAMPLE_PAYLOAD, SAMPLE_SIG_0_1_2, "0.1.2").unwrap();
        verify_signature(UPDATE_PUBLIC_KEY, SAMPLE_PAYLOAD, SAMPLE_SIG_0_1_2, "v0.1.2").unwrap();

        // 1-byte tamper in installer payload must fail.
        let mut tampered = SAMPLE_PAYLOAD.to_vec();
        tampered[0] ^= 0x01;
        assert!(verify_signature(UPDATE_PUBLIC_KEY, &tampered, SAMPLE_SIG_0_1_2, "0.1.2").is_err());

        // Manifest claiming a different version than the signed trusted comment must fail.
        let wrong_ver = verify_signature(UPDATE_PUBLIC_KEY, SAMPLE_PAYLOAD, SAMPLE_SIG_0_1_2, "0.1.3").unwrap_err();
        assert!(wrong_ver.contains("0.1.2") && wrong_ver.contains("0.1.3"), "{wrong_ver}");

        // Malformed public key fails cleanly.
        assert!(verify_signature("not-base64!!!", SAMPLE_PAYLOAD, SAMPLE_SIG_0_1_2, "0.1.2").is_err());
    }

    #[test]
    fn download_url_validation_requires_https_in_production() {
        assert!(validate_download_url("https://github.com/Kamo2323/wren/releases/download/v0.1.2/Wren.exe", false).is_ok());
        assert!(validate_download_url("http://github.com/Kamo2323/wren/releases/download/v0.1.2/Wren.exe", false).is_err());
        assert!(validate_download_url("http://127.0.0.1:8080/Wren.exe", false).is_err());
        assert!(validate_download_url("http://127.0.0.1:8080/Wren.exe", true).is_ok());
    }

    #[test]
    fn apply_script_uses_silent_update_relaunch_flags() {
        let s = apply_script_body();
        assert!(s.contains("Wait-Process -Id $ParentPid"));
        assert!(s.contains("'/S', '/UPDATE', '/R'"));
        assert!(s.contains("FallbackExe"));
        assert!(s.contains("$null = $proc.Handle"));
    }

    #[cfg(windows)]
    #[test]
    fn spawn_install_helper_executes_installer_logs_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("wren-upd-helper-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let marker = root.join("ran.txt");
        let log_path = root.join("logs").join("updater.log");
        let dummy_installer = root.join("dummy-setup.cmd");
        std::fs::write(
            &dummy_installer,
            format!("@echo off\r\necho %* > \"{}\"\r\nexit /b 0\r\n", marker.display()),
        )
        .unwrap();

        let mut dummy_parent = std::process::Command::new("cmd.exe")
            .args(["/C", "exit", "0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let parent_pid = dummy_parent.id();
        spawn_install_helper(&root, parent_pid, &dummy_installer, None, &log_path).unwrap();
        let _ = dummy_parent.wait();

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if log_path.exists()
                && !dummy_installer.exists()
                && !root.join("apply-update.ps1").exists()
                && marker.exists()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let ran = std::fs::read_to_string(&marker).unwrap_or_default();
        let installer_removed = !dummy_installer.exists();
        let script_removed = !root.join("apply-update.ps1").exists();
        let _ = std::fs::remove_dir_all(&root);

        assert!(log.contains("Installer finished with exit code 0"), "unexpected updater.log:\n{log}");
        assert!(ran.contains("/S") && ran.contains("/UPDATE") && ran.contains("/R"), "unexpected args: {ran}");
        assert!(installer_removed, "installer was not cleaned up");
        assert!(script_removed, "apply-update.ps1 was not cleaned up");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_spawn_install_helper_swaps_bundle_logs_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("wren-upd-mac-helper-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let target_app = root.join("Wren.app");
        std::fs::create_dir_all(target_app.join("Contents")).unwrap();
        std::fs::write(target_app.join("Contents").join("version.txt"), "0.1.0").unwrap();

        let src_dir = root.join("src");
        std::fs::create_dir_all(src_dir.join("Wren.app").join("Contents")).unwrap();
        std::fs::write(src_dir.join("Wren.app").join("Contents").join("version.txt"), "0.1.4").unwrap();

        let archive = root.join("Wren_0.1.4_aarch64.app.tar.gz");
        let tar_ok = std::process::Command::new("tar")
            .args(["-czf", archive.to_str().unwrap(), "-C", src_dir.to_str().unwrap(), "Wren.app"])
            .status()
            .unwrap()
            .success();
        assert!(tar_ok);

        let log_path = root.join("logs").join("updater.log");
        let mut dummy_parent = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let parent_pid = dummy_parent.id();
        spawn_macos_install_helper(&root, parent_pid, &archive, &target_app, &log_path, true).unwrap();
        let _ = dummy_parent.wait();

        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if log_path.exists() && !archive.exists() && !root.join("apply-update.sh").exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        let new_ver = std::fs::read_to_string(target_app.join("Contents").join("version.txt")).unwrap_or_default();
        let archive_removed = !archive.exists();
        let script_removed = !root.join("apply-update.sh").exists();
        let _ = std::fs::remove_dir_all(&root);

        assert!(log.contains("Installer finished with exit code 0"), "unexpected updater.log:\n{log}");
        assert_eq!(new_ver, "0.1.4");
        assert!(archive_removed, "archive was not cleaned up");
        assert!(script_removed, "apply-update.sh was not cleaned up");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resolve_macos_app_bundle_finds_enclosing_app() {
        assert_eq!(
            resolve_macos_app_bundle(Some(Path::new("/Applications/Wren.app/Contents/MacOS/wren"))),
            PathBuf::from("/Applications/Wren.app")
        );
        assert_eq!(
            resolve_macos_app_bundle(Some(Path::new("/Users/max/Dev/target/debug/wren"))),
            PathBuf::from("/Applications/Wren.app")
        );
    }

    #[derive(Clone)]
    struct RecordedReq {
        path: String,
        auth: Option<String>,
    }

    /// Tiny HTTP/1.1 test server that responds to a scripted set of routes and records headers.
    fn start_mock_server<F>(handler: F) -> (String, Arc<StdMutex<Vec<RecordedReq>>>)
    where
        F: Fn(&str, Option<&str>) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let base = format!("http://{addr}");
        let log = Arc::new(StdMutex::new(Vec::new()));
        let log_clone = log.clone();
        let handler = Arc::new(handler);

        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 16 * 1024];
                let Ok(n) = stream.read(&mut buf) else { continue };
                if n == 0 {
                    continue;
                }
                let text = String::from_utf8_lossy(&buf[..n]);
                let mut lines = text.lines();
                let path = lines
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_string();
                let mut auth = None;
                for l in lines {
                    if l.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = l.split_once(':')
                        && k.trim().eq_ignore_ascii_case("authorization")
                    {
                        auth = Some(v.trim().to_string());
                    }
                }
                log_clone.lock().unwrap().push(RecordedReq {
                    path: path.clone(),
                    auth: auth.clone(),
                });
                let (code, headers, body) = handler(&path, auth.as_deref());
                let reason = match code {
                    200 => "OK",
                    302 => "Found",
                    401 => "Unauthorized",
                    404 => "Not Found",
                    _ => "Error",
                };
                let mut resp = format!("HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                for (k, v) in headers {
                    resp.push_str(&format!("{k}: {v}\r\n"));
                }
                resp.push_str("\r\n");
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&body);
            }
        });

        (base, log)
    }

    #[test]
    fn public_repo_check_and_verified_download_succeed() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base_slot: Arc<std::sync::OnceLock<String>> = Arc::new(std::sync::OnceLock::new());
        let slot_for_handler = base_slot.clone();
        let (base, _reqs) = start_mock_server(move |path, _auth| {
            let b = slot_for_handler.get().unwrap();
            match path {
                "/releases/latest/download/latest.json" => {
                    let asset = format!("Wren_0.1.2{ASSET_SUFFIX}");
                    let body = serde_json::json!({
                        "version": "0.1.2",
                        "notes": "Fix microphone warm-up",
                        "pub_date": "2026-11-23T03:00:00Z",
                        "platforms": {
                            PLATFORM_KEY: {
                                "signature": SAMPLE_SIG_0_1_2,
                                "url": format!("{b}/releases/download/v0.1.2/{asset}")
                            }
                        }
                    })
                    .to_string();
                    (200, vec![("Content-Type".into(), "application/json".into())], body.into_bytes())
                }
                p if p == format!("/releases/download/v0.1.2/Wren_0.1.2{ASSET_SUFFIX}") => {
                    (200, vec![("Content-Type".into(), "application/octet-stream".into())], SAMPLE_PAYLOAD.to_vec())
                }
                _ => (404, vec![], vec![]),
            }
        });
        base_slot.set(base.clone()).unwrap();

        let cfg = UpdaterConfig {
            public_latest_url: format!("{base}/releases/latest/download/latest.json"),
            api_latest_release_url: format!("{base}/api/releases/latest"),
            public_key_b64: UPDATE_PUBLIC_KEY.into(),
            current_version: "0.1.0".into(),
            allow_insecure_localhost: true,
        };

        let result = rt.block_on(check_remote(&cfg, || None)).unwrap();
        let CheckResult::Available(pending) = result else {
            panic!("expected Available");
        };
        assert_eq!(pending.version, "0.1.2");
        assert_eq!(pending.notes.as_deref(), Some("Fix microphone warm-up"));

        let tmp = std::env::temp_dir().join(format!("wren-upd-pub-{}.exe.part", std::process::id()));
        let mut saw_progress = false;
        let bytes = rt
            .block_on(download_installer_to_file(
                &pending.source,
                None,
                "0.1.0",
                true,
                &tmp,
                |d, _| {
                    if d > 0 {
                        saw_progress = true;
                    }
                },
            ))
            .unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(saw_progress);
        verify_signature(&cfg.public_key_b64, &bytes, &pending.signature_b64, &pending.version).unwrap();
    }

    #[test]
    fn private_repo_fallback_uses_api_and_strips_auth_on_blob_redirect() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let base_slot: Arc<std::sync::OnceLock<String>> = Arc::new(std::sync::OnceLock::new());
        let slot_for_handler = base_slot.clone();

        let (base, reqs) = start_mock_server(move |path, auth| {
            let b = slot_for_handler.get().unwrap();
            match path {
                // Public endpoint returns 404 on a private repo.
                "/releases/latest/download/latest.json" => (404, vec![], vec![]),
                // GitHub REST API requires Bearer token.
                "/api/releases/latest" => {
                    if auth != Some("Bearer ghp_test_secret") {
                        return (401, vec![], vec![]);
                    }
                    let asset = format!("Wren_0.1.2{ASSET_SUFFIX}");
                    let body = serde_json::json!({
                        "tag_name": "v0.1.2",
                        "body": "Private release notes",
                        "html_url": "https://github.com/Kamo2323/wren/releases/tag/v0.1.2",
                        "published_at": "2026-11-23T03:00:00Z",
                        "assets": [
                            { "name": "latest.json", "url": format!("{b}/api/assets/1") },
                            { "name": asset, "url": format!("{b}/api/assets/2") }
                        ]
                    })
                    .to_string();
                    (200, vec![("Content-Type".into(), "application/json".into())], body.into_bytes())
                }
                "/api/assets/1" => {
                    if auth != Some("Bearer ghp_test_secret") {
                        return (401, vec![], vec![]);
                    }
                    (302, vec![("Location".into(), format!("{b}/blob/latest.json"))], vec![])
                }
                "/blob/latest.json" => {
                    // Simulated S3/Azure blob host: must NEVER receive Authorization header!
                    if auth.is_some() {
                        return (400, vec![], b"unexpected auth header".to_vec());
                    }
                    let asset = format!("Wren_0.1.2{ASSET_SUFFIX}");
                    let body = serde_json::json!({
                        "version": "0.1.2",
                        "notes": "Add in-app updater",
                        "platforms": {
                            PLATFORM_KEY: {
                                "signature": SAMPLE_SIG_0_1_2,
                                "url": format!("https://github.com/Kamo2323/wren/releases/download/v0.1.2/{asset}")
                            }
                        }
                    })
                    .to_string();
                    (200, vec![], body.into_bytes())
                }
                "/api/assets/2" => {
                    if auth != Some("Bearer ghp_test_secret") {
                        return (401, vec![], vec![]);
                    }
                    (302, vec![("Location".into(), format!("{b}/blob/setup.exe"))], vec![])
                }
                "/blob/setup.exe" => {
                    if auth.is_some() {
                        return (400, vec![], b"unexpected auth header".to_vec());
                    }
                    (200, vec![], SAMPLE_PAYLOAD.to_vec())
                }
                _ => (404, vec![], vec![]),
            }
        });
        base_slot.set(base.clone()).unwrap();

        let cfg = UpdaterConfig {
            public_latest_url: format!("{base}/releases/latest/download/latest.json"),
            api_latest_release_url: format!("{base}/api/releases/latest"),
            public_key_b64: UPDATE_PUBLIC_KEY.into(),
            current_version: "0.1.0".into(),
            allow_insecure_localhost: true,
        };

        let result = rt
            .block_on(check_remote(&cfg, || Some("ghp_test_secret".to_string())))
            .unwrap();
        let CheckResult::Available(pending) = result else {
            panic!("expected Available");
        };
        assert_eq!(pending.version, "0.1.2");
        assert!(matches!(pending.source, DownloadSource::GitHubAssetApi { .. }));

        let tmp = std::env::temp_dir().join(format!("wren-upd-priv-{}.exe.part", std::process::id()));
        let bytes = rt
            .block_on(download_installer_to_file(
                &pending.source,
                Some("ghp_test_secret"),
                "0.1.0",
                true,
                &tmp,
                |_, _| {},
            ))
            .unwrap();
        let _ = std::fs::remove_file(&tmp);
        verify_signature(&cfg.public_key_b64, &bytes, &pending.signature_b64, &pending.version).unwrap();

        let recorded = reqs.lock().unwrap();
        assert!(recorded.iter().any(|r| r.path == "/blob/latest.json" && r.auth.is_none()));
        assert!(recorded.iter().any(|r| r.path == "/blob/setup.exe" && r.auth.is_none()));
    }

    #[test]
    fn no_releases_yet_on_authenticated_private_repo_reports_up_to_date() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (base, _) = start_mock_server(|_path, _auth| (404, vec![], vec![]));
        let cfg = UpdaterConfig {
            public_latest_url: format!("{base}/releases/latest/download/latest.json"),
            api_latest_release_url: format!("{base}/api/releases/latest"),
            public_key_b64: UPDATE_PUBLIC_KEY.into(),
            current_version: "0.1.0".into(),
            allow_insecure_localhost: true,
        };
        let result = rt
            .block_on(check_remote(&cfg, || Some("ghp_test_secret".to_string())))
            .unwrap();
        assert!(matches!(result, CheckResult::UpToDate));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_apply_script_is_valid_sh_and_preserves_signing_identity() {
        let body = macos_apply_script_body();
        assert!(body.contains("/usr/bin/codesign --force --deep --sign"));
        assert!(body.contains("Preserving local code-signing identity"));
        let tmp = std::env::temp_dir().join(format!("wren-apply-syntax-{}.sh", std::process::id()));
        std::fs::write(&tmp, &body).unwrap();
        let status = std::process::Command::new("/bin/sh").args(["-n"]).arg(&tmp).status().unwrap();
        let _ = std::fs::remove_file(&tmp);
        assert!(status.success(), "apply-update.sh failed `/bin/sh -n` syntax check");
    }
}
