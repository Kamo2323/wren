//! First-run speech setup, inside the installed app (no terminal, no preinstalled uv or Python).
//!
//! What it installs is exactly what `scripts/setup-runtime.ps1` / `.sh` install, from the same pins
//! (`sidecar/runtime-manifest.json`): a private CPython (via a pinned, checksum-verified uv), the
//! sidecar venv from the frozen `uv.lock`, and the pinned model folders. Layout is unchanged, so a
//! runtime built by the scripts keeps working and is never rebuilt unless it is broken.
//!
//! Safety rules:
//! - The venv is built in `runtime/.venv.staging` and only swapped in after it imports cleanly; a
//!   failed or paused attempt never touches a working `.venv`.
//! - Model files download to `setup-cache/models/<name>/` (resumable `.part` files, SHA-256 checked)
//!   and the folder only replaces `models/<name>` once every file is verified. A folder Wren did not
//!   create (no `.wren-revision`) is moved aside, never deleted.
//! - Setup is "done" only when the real engine reports `Ready` after warming up on its clip.
//! - Network use is only here, only after the user presses the button. The engine stays offline.

pub mod download;
pub mod manifest;

use crate::engine::EngineStatus;
use crate::paths::Paths;
use crate::settings::EngineSettings;
use anyhow::{Context, Result, anyhow, bail};
use download::Cancelled;
use manifest::{ModelPins, PlatformPins, UvBuild};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Payload: `SetupStatus`, on every change (progress is throttled).
pub const EVENT_SETUP: &str = "app://runtime-setup";
const HF_BASE: &str = "https://huggingface.co";
/// Extra free space kept spare beyond the estimate.
const SPACE_MARGIN_BYTES: u64 = 300_000_000;
const EMIT_EVERY: Duration = Duration::from_millis(250);
const REVISION_MARKER: &str = ".wren-revision";
const RUNTIME_MARKER: &str = ".wren-runtime.json";

// ------------------------------------------------------------------------------------------ status

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub id: String,
    pub label: String,
    /// Download size (estimate for the runtime, exact for models).
    pub bytes: u64,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    /// False where Wren has no pinned runtime for this computer (Intel Mac, Windows on ARM, Linux).
    pub supported: bool,
    /// "windows", "macos" or "other".
    pub platform: String,
    /// Folder everything goes in (shown so the user knows where the space goes).
    pub location: String,
    pub parts: Vec<Part>,
    /// What still has to come down.
    pub download_bytes: u64,
    /// Free space needed while setting up (includes temporary files).
    pub needed_bytes: u64,
    /// Free space on that drive now (`None` if it could not be read).
    pub free_bytes: Option<u64>,
    pub log_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum SetupStatus {
    /// Everything is in place (or setup is not Wren's job here).
    NotNeeded,
    /// Something is missing; waiting for the user to start.
    Needed { reason: String, plan: Plan },
    Running {
        plan: Plan,
        step: String,
        step_label: String,
        step_index: u32,
        step_count: u32,
        /// 0..1 over the whole setup (weighted by bytes).
        fraction: f64,
        detail: String,
    },
    /// Stopped. `paused` = the user pressed Pause; otherwise `message` says what went wrong.
    Failed { plan: Plan, step: String, message: String, paused: bool },
    /// Files are in place; the real engine is loading and transcribing its warm-up clip.
    Starting { plan: Plan },
    /// The engine reported Ready after setup.
    Done { model: String, device: String },
}

// ------------------------------------------------------------------------------------------ layout

/// Where setup reads and writes.
#[derive(Debug, Clone)]
pub struct Layout {
    pub runtime: PathBuf,
    pub models: PathBuf,
    /// Temporary: uv itself, uv's package cache, the lock copy, half-downloaded models.
    pub cache: PathBuf,
    pub log: PathBuf,
    /// Bundled sidecar folder (holds the lock files).
    pub sidecar_dir: PathBuf,
    pub hf_base: String,
}

impl Layout {
    pub fn from_paths(paths: &Paths) -> Self {
        let runtime = paths.runtime_dir();
        let cache = runtime.parent().map(|p| p.join("setup-cache")).unwrap_or_else(|| paths.local.join("setup-cache"));
        Self {
            runtime,
            models: paths.models_dir(),
            cache,
            log: paths.logs_dir().join("runtime-setup.log"),
            sidecar_dir: crate::engine::resolve_sidecar_dir(),
            hf_base: HF_BASE.into(),
        }
    }

    pub fn venv(&self) -> PathBuf {
        self.runtime.join(".venv")
    }
    fn staging(&self) -> PathBuf {
        self.runtime.join(".venv.staging")
    }
    fn python_dir(&self) -> PathBuf {
        self.runtime.join("python")
    }
}

/// The interpreter inside a venv folder.
pub fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) { venv.join("Scripts").join("python.exe") } else { venv.join("bin").join("python") }
}

// ------------------------------------------------------------------------------- cheap checks

/// Why the engine cannot start yet, or `None` when its files are in place. Filesystem only (fast):
/// the supervisor asks before every launch, so a fresh install shows setup instead of a crash loop.
/// Only the pinned main model is checked; a custom model name is the user's own business.
pub fn engine_files_missing(python: &Path, models_dir: &Path, settings: &EngineSettings) -> Option<String> {
    if !python.is_file() {
        return Some("Speech recognition isn't set up on this computer yet.".into());
    }
    let (pins, _) = manifest::platform()?;
    if pins.models.iter().any(|m| *m == settings.model) {
        let model = &manifest::manifest().models[&settings.model];
        if !model.present_in(&models_dir.join(&settings.model)) {
            return Some("The speech model hasn't been downloaded yet.".into());
        }
    }
    None
}

/// Wren's own setup started (its temporary folder is still there) and a pinned model is still
/// missing: e.g. it was paused or Wren quit while the backup model downloaded. The main model may
/// already work, but setup stays open until everything it promised is in place.
fn unfinished(layout: &Layout) -> Option<String> {
    if !layout.cache.exists() {
        return None;
    }
    let (pins, _) = manifest::platform()?;
    let missing = pins.models.iter().any(|n| !manifest::manifest().models[n].present_in(&layout.models.join(n)));
    missing.then(|| "Speech setup didn't finish: some files are still to download.".to_string())
}

fn platform_label() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "other"
    }
}

/// What setup would do now (no network; quick).
pub fn plan(layout: &Layout) -> Plan {
    let free = free_space(&layout.runtime);
    let mut plan = Plan {
        supported: false,
        platform: platform_label().into(),
        location: layout.runtime.parent().unwrap_or(&layout.runtime).display().to_string(),
        parts: Vec::new(),
        download_bytes: 0,
        needed_bytes: 0,
        free_bytes: free,
        log_path: layout.log.display().to_string(),
    };
    let Some((pins, uv)) = manifest::platform() else { return plan };
    plan.supported = true;
    let venv_done = venv_python(&layout.venv()).is_file();
    plan.parts.push(Part {
        id: "runtime".into(),
        label: "Speech engine (Python and its packages)".into(),
        bytes: uv.size + pins.python_download_bytes + pins.packages_download_bytes,
        done: venv_done,
    });
    if !venv_done {
        plan.needed_bytes += uv.size + pins.packages_download_bytes + pins.runtime_disk_bytes;
    }
    for (i, name) in pins.models.iter().enumerate() {
        let m = &manifest::manifest().models[name];
        let done = m.present_in(&layout.models.join(name));
        plan.parts.push(Part {
            id: format!("model:{name}"),
            label: if i == 0 { format!("Speech model ({name})") } else { format!("Backup model for slower computers ({name})") },
            bytes: m.bytes(),
            done,
        });
        if !done {
            plan.needed_bytes += m.bytes();
        }
    }
    plan.download_bytes = plan.parts.iter().filter(|p| !p.done).map(|p| p.bytes).sum();
    if plan.needed_bytes > 0 {
        plan.needed_bytes += SPACE_MARGIN_BYTES;
    }
    plan
}

// ----------------------------------------------------------------------------------- service

/// What the app does around setup: stop the engine before files it may hold are replaced, and
/// start a fresh one once everything is in place.
pub struct Hooks {
    pub quiesce: Box<dyn Fn() + Send + Sync>,
    /// Start a fresh engine with the current settings: after a finished setup (its Ready completes
    /// setup), and after a run that stopped the engine and then stopped early.
    pub installed: Box<dyn Fn() + Send + Sync>,
}

type Emit = Box<dyn Fn(&SetupStatus) + Send + Sync>;

struct Inner {
    layout: Layout,
    status: Mutex<SetupStatus>,
    emit: Emit,
    cancel: AtomicBool,
    running: AtomicBool,
    /// This run stopped the engine (to replace files it may hold); a run that then stops early
    /// must start a fresh one so the app never sits on a stopped engine.
    quiesced: AtomicBool,
    worker: Mutex<Option<JoinHandle<()>>>,
}

/// Managed state behind the `runtime_setup_*` commands.
#[derive(Clone)]
pub struct Service {
    inner: Arc<Inner>,
}

impl Service {
    pub fn new(layout: Layout, python: &Path, settings: &EngineSettings, emit: Emit) -> Self {
        let initial = match engine_files_missing(python, &layout.models, settings).or_else(|| unfinished(&layout)) {
            Some(reason) => SetupStatus::Needed { reason, plan: plan(&layout) },
            None => SetupStatus::NotNeeded,
        };
        tracing::info!(needed = !matches!(initial, SetupStatus::NotNeeded), "speech setup check");
        Self {
            inner: Arc::new(Inner {
                layout,
                status: Mutex::new(initial),
                emit,
                cancel: AtomicBool::new(false),
                running: AtomicBool::new(false),
                quiesced: AtomicBool::new(false),
                worker: Mutex::new(None),
            }),
        }
    }

    pub fn status(&self) -> SetupStatus {
        // Free space changes while the screen is open: refresh the plan of an idle status.
        let mut s = self.inner.status.lock();
        if let SetupStatus::Needed { plan: p, .. } = &mut *s {
            *p = plan(&self.inner.layout);
        }
        s.clone()
    }

    pub fn is_running(&self) -> bool {
        self.inner.running.load(Ordering::SeqCst)
    }

    /// Start (or resume, or repair) in the background. `repair` also re-checks files that look present.
    pub fn start(&self, repair: bool, hooks: Hooks) -> Result<(), String> {
        if manifest::platform().is_none() {
            return Err("Automatic speech setup isn't available on this computer.".into());
        }
        if self.inner.running.swap(true, Ordering::SeqCst) {
            return Err("Speech setup is already running.".into());
        }
        self.inner.cancel.store(false, Ordering::SeqCst);
        self.inner.quiesced.store(false, Ordering::SeqCst);
        if let Some(old) = self.inner.worker.lock().take() {
            let _ = old.join();
        }
        let inner = self.inner.clone();
        let spawned = std::thread::Builder::new().name("wren-setup".into()).spawn(move || {
            let outcome = inner.run(repair, &hooks);
            match outcome {
                Ok(()) => {}
                Err((step, err)) => {
                    let paused = err.downcast_ref::<Cancelled>().is_some();
                    let message = if paused {
                        "Setup is paused. What has downloaded so far is kept, so it carries on from there.".to_string()
                    } else {
                        user_message(&step, &err)
                    };
                    inner.log(&format!("stopped at {step}: {err:#}"));
                    tracing::warn!(%step, paused, error = %format!("{err:#}"), "speech setup stopped");
                    inner.set(SetupStatus::Failed { plan: plan(&inner.layout), step, message, paused });
                    if inner.quiesced.swap(false, Ordering::SeqCst) {
                        // It reports "needs setup" (or runs, if the files it needs are complete).
                        (hooks.installed)();
                    }
                }
            }
            inner.running.store(false, Ordering::SeqCst);
        });
        match spawned {
            Ok(h) => {
                *self.inner.worker.lock() = Some(h);
                Ok(())
            }
            Err(e) => {
                self.inner.running.store(false, Ordering::SeqCst);
                Err(format!("Couldn't start speech setup ({e})."))
            }
        }
    }

    /// Pause: stops the current download or install step; finished parts stay.
    pub fn cancel(&self) {
        self.inner.cancel.store(true, Ordering::SeqCst);
    }

    /// Quit: pause and wait (briefly) for the worker to stop its child process.
    pub fn shutdown(&self, max: Duration) {
        self.cancel();
        let deadline = Instant::now() + max;
        while self.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Engine status changes: setup is only Done when the real engine says Ready.
    pub fn on_engine_status(&self, engine: &EngineStatus) {
        let next = {
            let s = self.inner.status.lock();
            match (&*s, engine) {
                (SetupStatus::Starting { .. }, EngineStatus::Ready { model, device, .. }) => {
                    Some(SetupStatus::Done { model: model.clone(), device: device.clone() })
                }
                (SetupStatus::Starting { plan }, EngineStatus::Failed { reason }) => Some(SetupStatus::Failed {
                    plan: plan.clone(),
                    step: "engine".into(),
                    message: format!("The speech engine didn't start: {reason}"),
                    paused: false,
                }),
                (SetupStatus::Starting { plan }, EngineStatus::NeedsSetup { reason }) => Some(SetupStatus::Failed {
                    plan: plan.clone(),
                    step: "engine".into(),
                    message: reason.clone(),
                    paused: false,
                }),
                (SetupStatus::NotNeeded, EngineStatus::NeedsSetup { reason }) if !self.is_running() => {
                    Some(SetupStatus::Needed { reason: reason.clone(), plan: plan(&self.inner.layout) })
                }
                // Files put in place another way (the developer scripts); not while Wren's own setup is unfinished.
                (SetupStatus::Needed { .. }, EngineStatus::Ready { .. }) if unfinished(&self.inner.layout).is_none() => {
                    Some(SetupStatus::NotNeeded)
                }
                _ => None,
            }
        };
        if let Some(n) = next {
            self.inner.set(n);
        }
    }
}

// ------------------------------------------------------------------------------------- the run

struct Step {
    id: &'static str,
    label: &'static str,
    weight: f64,
}

struct Progress<'a> {
    inner: &'a Inner,
    plan: Plan,
    steps: Vec<Step>,
    index: usize,
    done_weight: f64,
    total: f64,
    last: Instant,
}

impl<'a> Progress<'a> {
    fn new(inner: &'a Inner, plan: Plan, steps: Vec<Step>) -> Self {
        let total = steps.iter().map(|s| s.weight).sum::<f64>().max(1.0);
        Self { inner, plan, steps, index: 0, done_weight: 0.0, total, last: Instant::now() - EMIT_EVERY }
    }

    /// Move to step `id` (skipping any listed before it).
    fn begin(&mut self, id: &str, detail: &str) {
        while self.index < self.steps.len() && self.steps[self.index].id != id {
            self.done_weight += self.steps[self.index].weight;
            self.index += 1;
        }
        self.inner.log(&format!("step {id}"));
        self.emit(0.0, detail, true);
    }

    fn update(&mut self, step_fraction: f64, detail: &str) {
        self.emit(step_fraction, detail, false);
    }

    fn emit(&mut self, step_fraction: f64, detail: &str, force: bool) {
        if !force && self.last.elapsed() < EMIT_EVERY {
            return;
        }
        self.last = Instant::now();
        let Some(step) = self.steps.get(self.index) else { return };
        let fraction = ((self.done_weight + step.weight * step_fraction.clamp(0.0, 1.0)) / self.total).clamp(0.0, 0.999);
        self.inner.set(SetupStatus::Running {
            plan: self.plan.clone(),
            step: step.id.into(),
            step_label: step.label.into(),
            step_index: self.index as u32 + 1,
            step_count: self.steps.len() as u32,
            fraction,
            detail: detail.into(),
        });
    }
}

type StepError = (String, anyhow::Error);

trait StepContext<T> {
    fn at(self, step: &str) -> Result<T, StepError>;
}
impl<T> StepContext<T> for Result<T> {
    fn at(self, step: &str) -> Result<T, StepError> {
        self.map_err(|e| (step.to_string(), e))
    }
}

impl Inner {
    fn set(&self, s: SetupStatus) {
        let mut cur = self.status.lock();
        *cur = s.clone();
        (self.emit)(&s);
    }

    fn log(&self, line: &str) {
        if let Some(dir) = self.layout.log.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.layout.log) {
            let _ = writeln!(f, "{} {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        }
    }

    fn check_cancel(&self) -> Result<()> {
        if self.cancel.load(Ordering::SeqCst) { Err(Cancelled.into()) } else { Ok(()) }
    }

    fn run(&self, repair: bool, hooks: &Hooks) -> Result<(), StepError> {
        let (pins, uv) = manifest::platform().ok_or_else(|| ("check".to_string(), anyhow!("unsupported computer")))?;
        let layout = &self.layout;
        self.log(&format!("--- speech setup (repair: {repair}, uv {}, python {})", manifest::manifest().uv.version, manifest::manifest().python));
        let first_plan = plan(layout);
        {
            // Show something at once; the import check below can take a few seconds.
            let mut p = Progress::new(self, first_plan.clone(), vec![Step { id: "check", label: "Checking what's already here", weight: 1.0 }]);
            p.begin("check", "");
        }

        // 1. What is already usable?
        cleanup_stale(layout);
        let venv_ok = venv_python(&layout.venv()).is_file() && verify_imports(&venv_python(&layout.venv()), pins, &self.cancel).is_ok();
        self.check_cancel().at("check")?;
        let mut models_needed = Vec::new();
        for name in &pins.models {
            let m = &manifest::manifest().models[name];
            let dir = layout.models.join(name);
            let ok = if repair { model_verified(&dir, m, &self.cancel).at("check")? } else { m.present_in(&dir) };
            if !ok {
                models_needed.push(name.clone());
            }
        }
        self.log(&format!("runtime usable: {venv_ok}; models to fetch: {models_needed:?}"));

        // 2. Steps, weighted by what has to come down.
        let model_bytes: u64 = models_needed.iter().map(|n| manifest::manifest().models[n].bytes()).sum();
        let mut steps = vec![Step { id: "check", label: "Checking what's already here", weight: 0.01 }];
        if !venv_ok {
            steps.push(Step { id: "uv", label: "Getting the installer", weight: uv.size as f64 });
            steps.push(Step { id: "python", label: "Installing Python", weight: pins.python_download_bytes as f64 });
            steps.push(Step { id: "packages", label: "Installing the speech engine", weight: pins.packages_download_bytes as f64 });
        }
        if !models_needed.is_empty() {
            steps.push(Step { id: "models", label: "Downloading the speech models", weight: model_bytes as f64 });
        }
        let total: f64 = steps.iter().map(|s| s.weight).sum();
        steps[0].weight = (total * 0.01).max(1.0);
        steps.push(Step { id: "engine", label: "Starting the speech engine", weight: (total * 0.03).max(1.0) });
        let mut progress = Progress::new(self, first_plan, steps);
        progress.begin("check", "");

        // 3. Space, now that we know exactly what is needed.
        let mut needed = model_bytes;
        if !venv_ok {
            needed += uv.size + pins.packages_download_bytes + pins.runtime_disk_bytes;
        }
        if needed > 0 {
            needed += SPACE_MARGIN_BYTES;
            if let Some(free) = free_space(&layout.runtime)
                && free < needed
            {
                return Err((
                    "space".into(),
                    anyhow!(
                        "Wren needs about {} free for speech setup, but only {} is free on that drive. Free up some space, then try again.",
                        gb(needed),
                        gb(free)
                    ),
                ));
            }
        }

        // 4. The Python runtime.
        if !venv_ok {
            let client = download::client().at("uv")?;
            progress.begin("uv", &format!("uv {}", manifest::manifest().uv.version));
            let uv_exe = self.fetch_uv(&client, uv, &mut progress).at("uv")?;
            progress.begin("python", &format!("Python {}", manifest::manifest().python));
            self.install_python(&uv_exe).at("python")?;
            progress.begin("packages", "This is the biggest part and can take a while.");
            self.install_packages(&uv_exe, pins, &mut progress).at("packages")?;
            self.check_cancel().at("packages")?;
            self.quiesce(hooks);
            swap_venv(layout).at("packages")?;
            self.log("runtime swapped in");
            // The packages are in the venv now: uv's copy (about the same size again) can go at once.
            self.clean_uv_cache(&uv_exe);
        }

        // 5. Models.
        if !models_needed.is_empty() {
            let client = download::client().at("models")?;
            progress.begin("models", "");
            let total = model_bytes;
            let mut base = 0u64;
            for name in &models_needed {
                let m = &manifest::manifest().models[name];
                let mut on_progress = |done: u64| {
                    let all = base + done;
                    progress.update(all as f64 / total.max(1) as f64, &format!("{} of {}", mb(all), mb(total)));
                };
                install_model(&client, &layout.hf_base, name, m, &layout.models, &layout.cache, &self.cancel, &mut on_progress, &|| {
                    self.quiesce(hooks)
                })
                .at("models")?;
                base += m.bytes();
                self.log(&format!("model {name} installed"));
            }
        }

        // 6. Hand over to the engine; Done comes from `on_engine_status`.
        write_runtime_marker(layout, pins);
        progress.begin("engine", "Loading the model and transcribing a short test clip.");
        let final_plan = plan(layout);
        self.set(SetupStatus::Starting { plan: final_plan });
        self.log("files in place; starting the engine");
        (hooks.installed)();
        self.clean_cache();
        Ok(())
    }

    fn fetch_uv(&self, client: &reqwest::blocking::Client, uv: &UvBuild, progress: &mut Progress) -> Result<PathBuf> {
        let dir = self.layout.cache.join("uv");
        let exe_name = Path::new(&uv.exe).file_name().unwrap_or_default().to_os_string();
        let ready = dir.join("bin").join(&exe_name);
        let version_ok = |exe: &Path| -> bool {
            let mut cmd = Command::new(exe);
            cmd.arg("--version");
            hide_window(&mut cmd);
            cmd.output()
                .ok()
                .filter(|o| o.status.success())
                .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains(&manifest::manifest().uv.version))
        };
        if ready.is_file() && version_ok(&ready) {
            return Ok(ready);
        }
        let archive_name = uv.url.rsplit('/').next().unwrap_or("uv-archive");
        let archive = dir.join(archive_name);
        let size = uv.size;
        download::fetch(client, &uv.url, &archive, uv.size, &uv.sha256, &self.cancel, &mut |b| {
            progress.update(b as f64 / size as f64, &format!("{} of {}", mb(b), mb(size)))
        })?;
        let unpack = dir.join("unpack");
        let _ = std::fs::remove_dir_all(&unpack);
        std::fs::create_dir_all(&unpack)?;
        extract(&archive, &unpack)?;
        let found = find_file(&unpack, &exe_name).ok_or_else(|| anyhow!("{} was not in the downloaded archive", exe_name.to_string_lossy()))?;
        std::fs::create_dir_all(dir.join("bin"))?;
        let _ = std::fs::remove_file(&ready);
        std::fs::rename(&found, &ready).context("cannot move uv into place")?;
        let _ = std::fs::remove_dir_all(&unpack);
        if !version_ok(&ready) {
            bail!("the downloaded uv does not run on this computer");
        }
        Ok(ready)
    }

    fn uv_command(&self, uv: &Path) -> Command {
        let mut cmd = Command::new(uv);
        // A user's own uv / Python / pip settings must not leak into Wren's private runtime.
        for (key, _) in std::env::vars_os() {
            let k = key.to_string_lossy().to_ascii_uppercase();
            if k.starts_with("UV_") || k.starts_with("PYTHON") || k.starts_with("PIP_") || k == "VIRTUAL_ENV" || k == "CONDA_PREFIX" {
                cmd.env_remove(&key);
            }
        }
        cmd.env("UV_PYTHON_INSTALL_DIR", self.layout.python_dir())
            .env("UV_PYTHON_PREFERENCE", "only-managed")
            .env("UV_CACHE_DIR", self.layout.cache.join("uv-cache"))
            .env("UV_LINK_MODE", "copy") // the venv must not hard-link into a cache that gets deleted
            .env("UV_NO_CONFIG", "1")
            .env("UV_NO_PROGRESS", "1")
            .env("UV_PROJECT_ENVIRONMENT", self.layout.staging())
            .env("NO_COLOR", "1")
            .current_dir(&self.layout.runtime);
        cmd
    }

    fn install_python(&self, uv: &Path) -> Result<()> {
        std::fs::create_dir_all(&self.layout.runtime)?;
        let mut cmd = self.uv_command(uv);
        cmd.args(["python", "install", "--no-bin"]);
        if cfg!(windows) {
            cmd.arg("--no-registry"); // private: never in the registry or on PATH
        }
        cmd.arg(&manifest::manifest().python);
        self.run_tool(cmd, "Installing Python", None)
    }

    fn install_packages(&self, uv: &Path, pins: &PlatformPins, progress: &mut Progress) -> Result<()> {
        let layout = &self.layout;
        // The lock is copied out of the app bundle: uv never writes next to Wren's own files.
        let project_src = if pins.project.is_empty() { layout.sidecar_dir.clone() } else { layout.sidecar_dir.join(&pins.project) };
        let project = layout.cache.join("project");
        std::fs::create_dir_all(&project)?;
        for f in ["pyproject.toml", "uv.lock"] {
            std::fs::copy(project_src.join(f), project.join(f))
                .with_context(|| format!("the bundled {} is missing from {}", f, project_src.display()))?;
        }
        let staging = layout.staging();
        if !(venv_python(&staging).is_file() && python_runs(&venv_python(&staging))) {
            let mut cmd = self.uv_command(uv);
            cmd.args(["venv", "--clear", "--relocatable", "--python"]).arg(&manifest::manifest().python).arg(&staging);
            self.run_tool(cmd, "Creating the speech engine's environment", None)?;
        }
        // uv gives no byte counts, so progress is estimated from what lands on disk.
        let expect = (pins.runtime_disk_bytes * 2) as f64;
        let watch = [layout.cache.join("uv-cache"), staging.clone()];
        let mut cmd = self.uv_command(uv);
        cmd.args(["sync", "--frozen", "--no-dev", "--no-install-project", "--compile-bytecode", "--project"]).arg(&project);
        self.run_tool(
            cmd,
            "Installing the speech engine's packages",
            Some(&mut |_: Duration| {
                let on_disk: u64 = watch.iter().map(|d| dir_size(d)).sum();
                progress.update((on_disk as f64 / expect).min(0.97), &format!("{} on disk so far", mb(on_disk)));
            }),
        )?;
        progress.update(0.98, "Checking the install");
        verify_imports(&venv_python(&staging), pins, &self.cancel).context("the packages installed but the speech engine can't load")
    }

    /// Runs one tool to completion: output appended to the setup log, the last error lines kept for
    /// the message, killed at once on Pause. `tick` runs every half second while it works.
    fn run_tool(&self, mut cmd: Command, what: &str, mut tick: Option<&mut dyn FnMut(Duration)>) -> Result<()> {
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        hide_window(&mut cmd);
        self.log(&format!("run: {cmd:?}"));
        let mut child = cmd.spawn().with_context(|| format!("{what}: cannot start {}", cmd.get_program().to_string_lossy()))?;
        let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let mut readers = Vec::new();
        for stream in [child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>)]
            .into_iter()
            .flatten()
        {
            let tail = tail.clone();
            let log = self.layout.log.clone();
            readers.push(std::thread::spawn(move || {
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&log).ok();
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if let Some(f) = f.as_mut() {
                        let _ = writeln!(f, "    {line}");
                    }
                    let t = line.trim();
                    if !t.is_empty() {
                        let mut q = tail.lock();
                        q.push_back(t.to_string());
                        if q.len() > 6 {
                            q.pop_front();
                        }
                    }
                }
            }));
        }
        let started = Instant::now();
        let mut last_tick = Instant::now();
        let status = loop {
            if self.cancel.load(Ordering::SeqCst) {
                let _ = child.kill();
                let _ = child.wait();
                for r in readers {
                    let _ = r.join();
                }
                return Err(Cancelled.into());
            }
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if let Some(t) = tick.as_mut()
                && last_tick.elapsed() >= Duration::from_millis(500)
            {
                last_tick = Instant::now();
                t(started.elapsed());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        for r in readers {
            let _ = r.join();
        }
        if !status.success() {
            let lines: Vec<String> = tail.lock().iter().cloned().collect();
            let detail = lines.iter().rev().find(|l| l.to_ascii_lowercase().contains("error")).cloned().or_else(|| lines.last().cloned());
            bail!("{what} failed ({}){}", status, detail.map(|d| format!(": {}", clip(&d, 300))).unwrap_or_default());
        }
        Ok(())
    }

    fn quiesce(&self, hooks: &Hooks) {
        if !self.quiesced.swap(true, Ordering::SeqCst) {
            (hooks.quiesce)();
        }
    }

    fn clean_uv_cache(&self, uv: &Path) {
        let dir = self.layout.cache.join("uv-cache");
        if uv.is_file() && dir.exists() {
            let mut cmd = Command::new(uv);
            cmd.args(["cache", "clean"]).env("UV_CACHE_DIR", &dir).stdout(Stdio::null()).stderr(Stdio::null());
            hide_window(&mut cmd);
            let _ = cmd.status();
        }
        let _ = remove_tree(&dir);
    }

    /// After success the temporary files (uv, its package cache, staged downloads) go.
    fn clean_cache(&self) {
        let uv = self.layout.cache.join("uv").join("bin").join(if cfg!(windows) { "uv.exe" } else { "uv" });
        self.clean_uv_cache(&uv);
        if let Err(e) = remove_tree(&self.layout.cache) {
            self.log(&format!("could not remove {} ({e}); it is safe to delete", self.layout.cache.display()));
        }
    }
}

// --------------------------------------------------------------------------- building blocks

/// Leftovers of an interrupted swap. An old venv is removed only when a live one exists; if Wren
/// stopped between the two renames, the old one goes back. A staging venv is kept: uv carries on
/// filling it next time.
pub(crate) fn cleanup_stale(layout: &Layout) {
    let live = layout.venv();
    let Ok(entries) = std::fs::read_dir(&layout.runtime) else { return };
    let mut olds: Vec<PathBuf> =
        entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".venv.old-")).map(|e| e.path()).collect();
    olds.sort();
    while let Some(old) = olds.pop() {
        if !live.exists() && venv_python(&old).is_file() && std::fs::rename(&old, &live).is_ok() {
            tracing::warn!("restored the previous speech runtime after an interrupted update");
            continue;
        }
        let _ = remove_tree(&old);
    }
}

/// Put the verified staging venv live. The old one is renamed first and only removed once the new
/// one is in place; any failure puts the old one back.
pub(crate) fn swap_venv(layout: &Layout) -> Result<()> {
    let live = layout.venv();
    let staging = layout.staging();
    if !venv_python(&staging).is_file() {
        bail!("the new environment is incomplete");
    }
    let old = layout.runtime.join(format!(".venv.old-{}", chrono::Utc::now().timestamp_millis()));
    if live.exists() {
        retry_rename(&live, &old).context("the old speech engine files are in use (is another copy of Wren running?)")?;
    }
    if let Err(e) = retry_rename(&staging, &live) {
        if old.exists() {
            let _ = std::fs::rename(&old, &live);
        }
        return Err(e.context("cannot move the new speech engine into place"));
    }
    if old.exists() {
        let _ = remove_tree(&old);
    }
    Ok(())
}

fn retry_rename(from: &Path, to: &Path) -> Result<()> {
    let mut last = None;
    for _ in 0..10 {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                // Antivirus scanners and a just-stopped engine can hold files for a moment.
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    }
    Err(last.map(anyhow::Error::from).unwrap_or_else(|| anyhow!("rename failed")))
}

fn python_runs(python: &Path) -> bool {
    let mut cmd = Command::new(python);
    cmd.args(["-I", "-c", "import sys; print(sys.version_info[:2])"]).stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut cmd);
    cmd.status().is_ok_and(|s| s.success())
}

const VERIFY_PY: &str = r#"
import importlib, os, sys
from pathlib import Path
if os.name == "nt":
    root = Path(sys.prefix) / "Lib" / "site-packages" / "nvidia"
    for sub in ("cublas", "cudnn"):
        b = root / sub / "bin"
        if b.is_dir():
            os.add_dll_directory(str(b))
            os.environ["PATH"] = str(b) + os.pathsep + os.environ.get("PATH", "")
for name in sys.argv[1:]:
    importlib.import_module(name)
print("imports ok")
"#;

/// The venv really loads the engine's packages (not just "python.exe exists").
fn verify_imports(python: &Path, pins: &PlatformPins, cancel: &AtomicBool) -> Result<()> {
    let mut cmd = Command::new(python);
    cmd.args(["-I", "-c", VERIFY_PY]).args(&pins.verify_imports).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().to_ascii_uppercase().starts_with("PYTHON") {
            cmd.env_remove(&key);
        }
    }
    cmd.env("PYTHONDONTWRITEBYTECODE", "1");
    hide_window(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("cannot run {}", python.display()))?;
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Cancelled.into());
        }
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut e, &mut err);
            }
            let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
            bail!("import check failed ({status}){}", if last.is_empty() { String::new() } else { format!(": {}", clip(&last, 200)) });
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the import check took too long");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Every pinned file is present with the right size and hash (repair only; reads the whole model).
fn model_verified(dir: &Path, m: &ModelPins, cancel: &AtomicBool) -> Result<bool> {
    for f in &m.files {
        if !download::file_matches(&dir.join(&f.name), f.size, &f.sha256, cancel)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Download (resuming) every file of one model into the cache, verify each, then put the folder
/// in place. Files already verified in a Wren-made folder are reused rather than downloaded again.
#[allow(clippy::too_many_arguments)]
pub(crate) fn install_model(
    client: &reqwest::blocking::Client,
    base: &str,
    name: &str,
    m: &ModelPins,
    models_dir: &Path,
    cache: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
    quiesce: &dyn Fn(),
) -> Result<()> {
    let dest = models_dir.join(name);
    let stage = cache.join("models").join(name);
    std::fs::create_dir_all(&stage)?;
    let wren_made = dest.join(REVISION_MARKER).is_file();
    let mut done = 0u64;
    for f in &m.files {
        let staged = stage.join(&f.name);
        let existing = dest.join(&f.name);
        if wren_made && !staged.exists() && download::file_matches(&existing, f.size, &f.sha256, cancel)? {
            std::fs::copy(&existing, &staged).with_context(|| format!("cannot reuse {}", existing.display()))?;
        } else {
            let start = done;
            download::fetch(client, &m.file_url(base, &f.name), &staged, f.size, &f.sha256, cancel, &mut |b| progress(start + b))?;
        }
        done += f.size;
        progress(done);
    }
    std::fs::write(stage.join(REVISION_MARKER), format!("{}\n", m.revision))?;
    quiesce();
    std::fs::create_dir_all(models_dir)?;
    if dest.exists() {
        if wren_made {
            remove_tree(&dest).with_context(|| format!("cannot replace {} (is it in use?)", dest.display()))?;
        } else {
            // Not ours: keep it, out of the way.
            let aside = models_dir.join(format!("{name}.user-backup-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            retry_rename(&dest, &aside).with_context(|| format!("cannot move {} aside", dest.display()))?;
            tracing::warn!(from = %dest.display(), to = %aside.display(), "a model folder Wren did not make was moved aside");
        }
    }
    retry_rename(&stage, &dest).with_context(|| format!("cannot move the {name} model into place"))?;
    Ok(())
}

fn write_runtime_marker(layout: &Layout, pins: &PlatformPins) {
    let project_src = if pins.project.is_empty() { layout.sidecar_dir.clone() } else { layout.sidecar_dir.join(&pins.project) };
    let lock_sha = download::sha256_file(&project_src.join("uv.lock"), &AtomicBool::new(false)).unwrap_or_default();
    let marker = serde_json::json!({
        "uv": manifest::manifest().uv.version,
        "python": manifest::manifest().python,
        "lockSha256": lock_sha,
        "platform": manifest::platform_key(),
        "app": env!("CARGO_PKG_VERSION"),
        "installedAt": chrono::Utc::now().to_rfc3339(),
    });
    let _ = std::fs::write(layout.runtime.join(RUNTIME_MARKER), serde_json::to_string_pretty(&marker).unwrap_or_default());
}

/// Unpacks with the system `tar` (bsdtar reads .zip on Windows 10+; /usr/bin/tar on macOS), with
/// PowerShell's Expand-Archive as a Windows fallback.
fn extract(archive: &Path, into: &Path) -> Result<()> {
    #[cfg(windows)]
    let tar = std::env::var_os("SystemRoot").map(|r| PathBuf::from(r).join("System32").join("tar.exe")).unwrap_or_else(|| PathBuf::from("tar.exe"));
    #[cfg(not(windows))]
    let tar = PathBuf::from("/usr/bin/tar");
    let mut cmd = Command::new(&tar);
    cmd.arg("-xf").arg(archive).arg("-C").arg(into).stdout(Stdio::null()).stderr(Stdio::null());
    hide_window(&mut cmd);
    if cmd.status().is_ok_and(|s| s.success()) {
        return Ok(());
    }
    #[cfg(windows)]
    {
        let mut ps = Command::new("powershell.exe");
        ps.args(["-NoProfile", "-NonInteractive", "-Command", "Expand-Archive -LiteralPath $env:WREN_ARCHIVE -DestinationPath $env:WREN_UNPACK -Force"])
            .env("WREN_ARCHIVE", archive)
            .env("WREN_UNPACK", into)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_window(&mut ps);
        if ps.status().is_ok_and(|s| s.success()) {
            return Ok(());
        }
    }
    bail!("cannot unpack {}", archive.display())
}

fn find_file(dir: &Path, name: &std::ffi::OsStr) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name() == Some(name) {
            return Some(p);
        }
    }
    None
}

pub(crate) fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

/// `remove_dir_all` that also clears read-only flags (uv marks some cache files read-only).
pub(crate) fn remove_tree(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    if std::fs::remove_dir_all(path).is_ok() {
        return Ok(());
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.flatten() {
                let p = e.path();
                if let Ok(meta) = std::fs::symlink_metadata(&p) {
                    if meta.is_dir() {
                        stack.push(p.clone());
                    }
                    let mut perms = meta.permissions();
                    if perms.readonly() {
                        #[allow(clippy::permissions_set_readonly_false)]
                        perms.set_readonly(false);
                        let _ = std::fs::set_permissions(&p, perms);
                    }
                }
            }
        }
    }
    std::fs::remove_dir_all(path)
}

/// Free bytes on the drive holding `path` (the nearest existing folder above it).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path.to_path_buf();
    while !p.exists() {
        p = p.parent()?.to_path_buf();
    }
    free_space_at(&p)
}

#[cfg(windows)]
fn free_space_at(p: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = p.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(windows::core::PCWSTR(wide.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

#[cfg(unix)]
fn free_space_at(p: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return None;
    }
    let st = unsafe { st.assume_init() };
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(not(any(windows, unix)))]
fn free_space_at(_: &Path) -> Option<u64> {
    None
}

fn hide_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max).collect::<String>()) }
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 { format!("{:.2} GB", bytes as f64 / 1e9) } else { format!("{} MB", bytes / 1_000_000) }
}

fn gb(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// One sentence for the setup screen; the full detail is in the setup log.
fn user_message(step: &str, err: &anyhow::Error) -> String {
    let detail = clip(&format!("{err:#}"), 400);
    match step {
        "space" => detail,
        "uv" => format!("Couldn't download the installer: {detail}"),
        "python" => format!("Couldn't install Python: {detail}"),
        "packages" => format!("Couldn't install the speech engine: {detail}"),
        "models" => format!("Couldn't download the speech models: {detail}"),
        _ => format!("Speech setup stopped: {detail}"),
    }
}

#[cfg(test)]
mod tests;
