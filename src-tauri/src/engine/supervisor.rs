//! Owns the sidecar process: launch (CREATE_NO_WINDOW, suspended until it is inside the Job Object),
//! the stdout reader, the request map, and restarts with back-off.
//!
//! Why suspended: the venv's `python.exe` is a launcher that starts the real interpreter as a child.
//! A child created before the launcher joins the Job Object would escape it, so the launcher is
//! created suspended, assigned to the job, and only then resumed. Children inherit the job.
//!
//! Failure policy (spec §5, constants): the n-th failure within `SIDECAR_FAILURE_WINDOW_MS` waits
//! `SIDECAR_RESTART_BACKOFF_MS[n-1]` (last step repeats); `SIDECAR_MAX_FAILURES` in the window →
//! `Failed` until `restart()`. Crashing before `ready` `SIDECAR_CPU_AFTER_PRE_READY_CRASHES` times in a
//! row (CUDA can abort the process instead of raising) → the next launch is forced onto the CPU model.

use super::protocol::{self, ErrorCode, Incoming, LogLevel, Request};
use super::{EngineConfig, EngineError, EngineStatus, TranscribeRequest, TranscribeResult};
use crate::constants::{
    SIDECAR_CPU_AFTER_PRE_READY_CRASHES, SIDECAR_FAILURE_WINDOW_MS, SIDECAR_MAX_FAILURES, SIDECAR_POLL_MS,
    SIDECAR_READY_TIMEOUT_MS, SIDECAR_RESTART_BACKOFF_MS, SIDECAR_SHUTDOWN_GRACE_MS,
};
use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::{Receiver, Sender, select};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The sidecar log is moved to `sidecar.log.old` when it grows past this at a launch.
const SIDECAR_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Exit code the sidecar uses when no model could be loaded at all.
const EXIT_NO_MODEL: i32 = 3;

type Reply = Result<TranscribeResult, EngineError>;

enum Control {
    Restart,
    Shutdown,
}

enum Event {
    Ready,
    Eof,
}

enum Outcome {
    Shutdown,
    Restart,
    Failed { reason: String, was_ready: bool },
}

struct Running {
    child: Child,
    events: Receiver<Event>,
}

pub(super) struct Supervisor {
    cfg: EngineConfig,
    status: Mutex<EngineStatus>,
    status_tx: Sender<EngineStatus>,
    pending: Mutex<HashMap<String, Sender<Reply>>>,
    stdin: Mutex<Option<ChildStdin>>,
    pong: Mutex<Option<Sender<()>>>,
    /// Last warn/error `log` line from the sidecar (fixed wording, no dictated text), for `Failed`.
    last_problem: Mutex<Option<String>>,
    /// Bumped at every launch; a reader from an older process never changes the status.
    generation: AtomicU64,
    /// Pid of the current venv launcher (0 = none).
    launcher_pid: AtomicU64,
    job: Job,
    control: Sender<Control>,
    control_rx: Receiver<Control>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Supervisor {
    pub(super) fn start(cfg: EngineConfig, status_tx: Sender<EngineStatus>) -> Arc<Self> {
        let (control, control_rx) = crossbeam_channel::unbounded();
        let job = Job::new();
        let sup = Arc::new(Self {
            cfg,
            status: Mutex::new(EngineStatus::Starting),
            status_tx,
            pending: Mutex::new(HashMap::new()),
            stdin: Mutex::new(None),
            pong: Mutex::new(None),
            last_problem: Mutex::new(None),
            generation: AtomicU64::new(0),
            launcher_pid: AtomicU64::new(0),
            job: match job {
                Ok(j) => j,
                Err(err) => {
                    tracing::error!(error = %err, "cannot create the sidecar Job Object");
                    Job::none()
                }
            },
            control,
            control_rx,
            thread: Mutex::new(None),
        });
        let _ = sup.status_tx.send(EngineStatus::Starting);
        let runner = sup.clone();
        match std::thread::Builder::new().name("wren-engine".into()).spawn(move || runner.supervise()) {
            Ok(handle) => *sup.thread.lock() = Some(handle),
            Err(err) => sup.set_status(EngineStatus::Failed { reason: format!("The speech engine could not start ({err}).") }),
        }
        sup
    }

    pub(super) fn status(&self) -> EngineStatus {
        self.status.lock().clone()
    }

    fn set_status(&self, s: EngineStatus) {
        let mut cur = self.status.lock();
        if *cur != s {
            *cur = s.clone();
            // Sent under the lock so listeners see changes in order.
            let _ = self.status_tx.send(s);
        }
    }

    pub(super) fn transcribe(&self, req: TranscribeRequest, timeout: Duration) -> Reply {
        if !matches!(*self.status.lock(), EngineStatus::Ready { .. }) {
            return Err(EngineError::NotReady);
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        {
            let mut pending = self.pending.lock();
            if pending.contains_key(&req.id) {
                return Err(EngineError::Internal("duplicate request id".into()));
            }
            pending.insert(req.id.clone(), tx);
        }
        let line = protocol::encode(&Request::Transcribe {
            id: req.id.clone(),
            audio_path: req.audio_path.display().to_string(),
            bias_terms: req.bias_terms,
            prompt: req.prompt,
        });
        if let Err(err) = self.write_line(&line) {
            self.pending.lock().remove(&req.id);
            return Err(err);
        }
        match rx.recv_timeout(timeout) {
            Ok(reply) => reply,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                self.pending.lock().remove(&req.id);
                Err(EngineError::Timeout)
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                Err(EngineError::Internal("the speech engine stopped".into()))
            }
        }
    }

    pub(super) fn ping(&self, timeout: Duration) -> bool {
        let (tx, rx) = crossbeam_channel::bounded(1);
        *self.pong.lock() = Some(tx);
        if self.write_line(&protocol::encode(&Request::Ping)).is_err() {
            return false;
        }
        rx.recv_timeout(timeout).is_ok()
    }

    pub(super) fn restart(&self) {
        let _ = self.control.send(Control::Restart);
    }

    pub(super) fn shutdown(&self) {
        let handle = self.thread.lock().take();
        if let Some(handle) = handle {
            let _ = self.control.send(Control::Shutdown);
            let _ = handle.join();
        }
        // Belt and braces: nothing may outlive Wren in the job.
        self.job.terminate();
    }

    /// Launcher first, then the interpreter it started.
    pub(super) fn process_ids(&self) -> Vec<u32> {
        let mut ids = self.job.process_ids();
        let launcher = self.launcher_pid.load(Ordering::SeqCst) as u32;
        if let Some(pos) = ids.iter().position(|&id| id == launcher) {
            ids.swap(0, pos);
        }
        ids
    }

    fn write_line(&self, line: &str) -> Result<(), EngineError> {
        let mut stdin = self.stdin.lock();
        let pipe = stdin.as_mut().ok_or(EngineError::NotReady)?;
        pipe.write_all(line.as_bytes())
            .and_then(|_| pipe.flush())
            .map_err(|e| EngineError::Internal(format!("cannot reach the speech engine ({})", e.kind())))
    }

    fn fail_pending(&self) {
        let drained: Vec<_> = self.pending.lock().drain().collect();
        for (_, tx) in drained {
            let _ = tx.send(Err(EngineError::Internal("the speech engine stopped".into())));
        }
    }

    // ------------------------------------------------------------------------------- supervision

    fn supervise(self: Arc<Self>) {
        let window = Duration::from_millis(SIDECAR_FAILURE_WINDOW_MS);
        let mut failures: VecDeque<Instant> = VecDeque::new();
        let mut pre_ready_crashes = 0usize;
        loop {
            let force_cpu = pre_ready_crashes >= SIDECAR_CPU_AFTER_PRE_READY_CRASHES;
            if force_cpu {
                tracing::warn!(pre_ready_crashes, "speech engine keeps crashing while loading; trying the CPU model");
            }
            // First run: no runtime or model yet. Wait for setup (it restarts us) instead of failing
            // in a loop; this is not a failure and does not count towards giving up.
            if let Some(reason) = crate::runtime_setup::engine_files_missing(&self.cfg.python, &self.cfg.models_dir, &self.cfg.settings) {
                tracing::info!(%reason, "speech engine waiting for setup");
                self.set_status(EngineStatus::NeedsSetup { reason });
                match self.control_rx.recv() {
                    Ok(Control::Restart) => {
                        failures.clear();
                        pre_ready_crashes = 0;
                        continue;
                    }
                    Ok(Control::Shutdown) | Err(_) => break,
                }
            }
            self.set_status(EngineStatus::Starting);
            *self.last_problem.lock() = None;
            let outcome = match self.launch(force_cpu) {
                Ok(running) => {
                    self.set_status(EngineStatus::WarmingUp);
                    self.watch(running)
                }
                Err(err) => Outcome::Failed { reason: format!("{err:#}"), was_ready: false },
            };
            *self.stdin.lock() = None;
            self.fail_pending();
            let reason = match outcome {
                Outcome::Shutdown => break,
                Outcome::Restart => {
                    tracing::info!("speech engine restart requested");
                    failures.clear();
                    pre_ready_crashes = 0;
                    continue;
                }
                Outcome::Failed { reason, was_ready } => {
                    // Say so at once: the sidecar is gone, so nothing may look Ready during the back-off.
                    // (Becomes Failed below instead if this was one failure too many.)
                    self.set_status(EngineStatus::Starting);
                    pre_ready_crashes = if was_ready { 0 } else { pre_ready_crashes + 1 };
                    reason
                }
            };
            let now = Instant::now();
            failures.push_back(now);
            while failures.front().is_some_and(|t| now.duration_since(*t) > window) {
                failures.pop_front();
            }
            tracing::warn!(failures = failures.len(), %reason, "speech engine stopped");
            if failures.len() >= SIDECAR_MAX_FAILURES {
                self.set_status(EngineStatus::Failed {
                    reason: format!(
                        "The speech engine stopped {} times in {} minutes, so Wren has given up for now ({reason}). Restart it from Settings.",
                        failures.len(),
                        SIDECAR_FAILURE_WINDOW_MS / 60_000
                    ),
                });
                match self.control_rx.recv() {
                    Ok(Control::Restart) => {
                        failures.clear();
                        pre_ready_crashes = 0;
                        continue;
                    }
                    Ok(Control::Shutdown) | Err(_) => break,
                }
            }
            let step = (failures.len() - 1).min(SIDECAR_RESTART_BACKOFF_MS.len() - 1);
            match self.control_rx.recv_timeout(Duration::from_millis(SIDECAR_RESTART_BACKOFF_MS[step])) {
                Ok(Control::Restart) => {
                    failures.clear();
                    pre_ready_crashes = 0;
                }
                Ok(Control::Shutdown) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            }
        }
        self.job.terminate();
        self.set_status(EngineStatus::Failed { reason: "The speech engine has been shut down.".into() });
    }

    fn watch(&self, mut running: Running) -> Outcome {
        let started = Instant::now();
        let mut was_ready = false;
        let mut stdout_closed = false;
        loop {
            select! {
                recv(self.control_rx) -> msg => match msg {
                    Ok(Control::Restart) => {
                        self.kill(&mut running);
                        return Outcome::Restart;
                    }
                    Ok(Control::Shutdown) | Err(_) => {
                        self.stop_gracefully(&mut running);
                        return Outcome::Shutdown;
                    }
                },
                recv(running.events) -> ev => match ev {
                    Ok(Event::Ready) => was_ready = true,
                    Ok(Event::Eof) | Err(_) => stdout_closed = true,
                },
                default(Duration::from_millis(SIDECAR_POLL_MS)) => {}
            }
            match running.child.try_wait() {
                Ok(Some(status)) => {
                    // The launcher has gone; make sure the interpreter has too.
                    self.job.terminate();
                    let reason = match (status.code(), self.last_problem.lock().clone()) {
                        (Some(EXIT_NO_MODEL), Some(p)) => format!("no speech model could be loaded: {p}"),
                        (Some(EXIT_NO_MODEL), None) => "no speech model could be loaded".into(),
                        (Some(code), _) => format!("the speech engine exited with code {code}"),
                        (None, _) => "the speech engine exited".into(),
                    };
                    return Outcome::Failed { reason, was_ready };
                }
                Ok(None) => {}
                Err(err) => {
                    self.kill(&mut running);
                    return Outcome::Failed { reason: format!("cannot watch the speech engine ({err})"), was_ready };
                }
            }
            if stdout_closed {
                // Output closed but the launcher lingers: the interpreter died under it.
                self.kill(&mut running);
                return Outcome::Failed { reason: "the speech engine closed its output".into(), was_ready };
            }
            if !was_ready && started.elapsed() > Duration::from_millis(SIDECAR_READY_TIMEOUT_MS) {
                self.kill(&mut running);
                return Outcome::Failed { reason: "the speech engine did not become ready in time".into(), was_ready };
            }
        }
    }

    fn kill(&self, running: &mut Running) {
        self.job.terminate();
        let _ = running.child.kill();
        let _ = running.child.wait();
    }

    fn stop_gracefully(&self, running: &mut Running) {
        let _ = self.write_line(&protocol::encode(&Request::Shutdown));
        *self.stdin.lock() = None; // EOF also tells the sidecar to leave
        let deadline = Instant::now() + Duration::from_millis(SIDECAR_SHUTDOWN_GRACE_MS);
        while Instant::now() < deadline {
            if matches!(running.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(Duration::from_millis(SIDECAR_POLL_MS));
        }
        self.kill(running);
    }

    fn launch(self: &Arc<Self>, force_cpu: bool) -> Result<Running> {
        let cfg = &self.cfg;
        if !cfg.python.is_file() {
            #[cfg(windows)]
            let setup_cmd = "scripts\\setup-runtime.ps1";
            #[cfg(not(windows))]
            let setup_cmd = "scripts/setup-runtime.sh";
            bail!("the speech runtime is not installed ({} is missing; run {setup_cmd})", cfg.python.display());
        }
        if !cfg.sidecar_dir.join("wren_sidecar").join("__main__.py").is_file() {
            bail!("the speech engine files are missing from {}", cfg.sidecar_dir.display());
        }
        let log = open_sidecar_log(&cfg.log_file)?;

        let mut cmd = Command::new(&cfg.python);
        cmd.args(cfg.sidecar_args(force_cpu))
            .current_dir(&cfg.sidecar_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        for var in ["PYTHONHOME", "PYTHONPATH", "PYTHONSTARTUP", "PYTHONUSERBASE", "PYTHONEXECUTABLE", "VIRTUAL_ENV"] {
            cmd.env_remove(var);
        }
        cmd.env("PYTHONPATH", &cfg.sidecar_dir)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            // Nothing may reach the internet at runtime (models are local_files_only as well).
            .env("HF_HUB_OFFLINE", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env("TRANSFORMERS_OFFLINE", "1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            const CREATE_SUSPENDED: u32 = 0x0000_0004;
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Put the sidecar in its own process group so shutdown/kill tears down any children too.
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().context("cannot launch the speech engine")?;
        let pid = child.id();
        self.launcher_pid.store(pid as u64, Ordering::SeqCst);
        if let Err(err) = self.job.assign(&child).and_then(|_| resume_process(pid)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(err.context("cannot place the speech engine in its job"));
        }
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin pipe"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout pipe"))?;
        *self.stdin.lock() = Some(stdin);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (events_tx, events) = crossbeam_channel::unbounded();
        let me = self.clone();
        std::thread::Builder::new()
            .name("wren-engine-reader".into())
            .spawn(move || me.read_stdout(stdout, generation, events_tx))
            .context("cannot start the engine reader thread")?;
        tracing::info!(pid, force_cpu, model = %cfg.settings.model, "speech engine launched");
        Ok(Running { child, events })
    }

    fn read_stdout(self: Arc<Self>, stdout: ChildStdout, generation: u64, events: Sender<Event>) {
        let mut reader = BufReader::new(stdout);
        let mut raw = Vec::new();
        loop {
            raw.clear();
            // Bytes, not read_line: one non-UTF-8 byte must cost one line, not the whole engine.
            match reader.read_until(b'\n', &mut raw) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let Ok(line) = std::str::from_utf8(&raw) else {
                tracing::warn!("non-UTF-8 line from the speech engine skipped");
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            let msg = match protocol::parse(line) {
                Ok(m) => m,
                Err(err) => {
                    // Never log the line: it could be a result.
                    tracing::warn!(error = %err, "unreadable line from the speech engine");
                    continue;
                }
            };
            let current = self.generation.load(Ordering::SeqCst) == generation;
            match msg {
                Incoming::Ready { model, device, compute, vram_mb, .. } => {
                    tracing::info!(%model, %device, %compute, vram_mb, "speech engine ready");
                    if current {
                        self.set_status(EngineStatus::Ready { model, device, vram_mb });
                    }
                    let _ = events.send(Event::Ready);
                }
                Incoming::Result { id, text, speech_ms, infer_ms, segments } => {
                    if let Some(tx) = self.pending.lock().remove(&id) {
                        let _ = tx.send(Ok(TranscribeResult { text, speech_ms, infer_ms, segments }));
                    }
                }
                Incoming::Error { id: Some(id), code, msg } => {
                    tracing::warn!(?code, %msg, "speech engine request failed");
                    let err = match code {
                        ErrorCode::Oom => EngineError::Oom(msg),
                        ErrorCode::BadAudio => EngineError::BadAudio(msg),
                        ErrorCode::NotReady => EngineError::NotReady,
                        ErrorCode::Internal => EngineError::Internal(msg),
                    };
                    if let Some(tx) = self.pending.lock().remove(&id) {
                        let _ = tx.send(Err(err));
                    }
                }
                Incoming::Error { id: None, code, msg } => tracing::warn!(?code, %msg, "speech engine error"),
                Incoming::Log { level, msg } => {
                    let msg: String = msg.chars().take(300).collect();
                    match level {
                        LogLevel::Info => tracing::info!(target: "wren::sidecar", %msg),
                        LogLevel::Warn => {
                            tracing::warn!(target: "wren::sidecar", %msg);
                            *self.last_problem.lock() = Some(msg);
                        }
                        LogLevel::Error => {
                            tracing::error!(target: "wren::sidecar", %msg);
                            *self.last_problem.lock() = Some(msg);
                        }
                    }
                }
                Incoming::Ping => {
                    let _ = self.write_line(&protocol::encode(&Request::Pong));
                }
                Incoming::Pong => {
                    if let Some(tx) = self.pong.lock().take() {
                        let _ = tx.send(());
                    }
                }
                Incoming::Shutdown => {}
            }
        }
        let _ = events.send(Event::Eof);
    }
}

fn open_sidecar_log(path: &std::path::Path) -> Result<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    if std::fs::metadata(path).map(|m| m.len() > SIDECAR_LOG_MAX_BYTES).unwrap_or(false) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let _ = writeln!(f, "--- sidecar launch {} ---", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    Ok(f)
}

// ------------------------------------------------------------------------------------- Process lifecycle

/// Windows: a Job Object with KILL_ON_JOB_CLOSE (when Wren exits or crashes, Windows kills every
/// process in it). Unix/macOS: tracks the sidecar's process group (`pgid`) and sends `SIGKILL` to
/// the whole group on terminate/drop (and closing stdin already makes the sidecar `os._exit(0)`).
#[cfg(windows)]
struct Job(Option<isize>);
#[cfg(not(windows))]
struct Job(Mutex<Option<i32>>);

// Kernel object handles / pgid mutex may be used from any thread.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

#[cfg(windows)]
mod win {
    pub use windows::Win32::Foundation::{CloseHandle, HANDLE};
    pub use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    pub use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation,
        QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
    };
    pub use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
}

impl Job {
    fn none() -> Self {
        #[cfg(windows)]
        {
            Job(None)
        }
        #[cfg(not(windows))]
        {
            Job(Mutex::new(None))
        }
    }

    #[cfg(windows)]
    fn new() -> Result<Self> {
        use win::*;
        unsafe {
            let handle = CreateJobObjectW(None, windows::core::PCWSTR::null()).context("CreateJobObjectW")?;
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if let Err(err) = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) {
                let _ = CloseHandle(handle);
                return Err(anyhow!("SetInformationJobObject: {err}"));
            }
            Ok(Job(Some(handle.0 as isize)))
        }
    }

    #[cfg(not(windows))]
    fn new() -> Result<Self> {
        Ok(Job(Mutex::new(None)))
    }

    #[cfg(windows)]
    fn handle(&self) -> Option<win::HANDLE> {
        self.0.map(|h| win::HANDLE(h as *mut core::ffi::c_void))
    }

    #[cfg(windows)]
    fn assign(&self, child: &Child) -> Result<()> {
        use std::os::windows::io::AsRawHandle;
        let job = self.handle().ok_or_else(|| anyhow!("no Job Object"))?;
        let process = win::HANDLE(child.as_raw_handle());
        unsafe { win::AssignProcessToJobObject(job, process) }.context("AssignProcessToJobObject")
    }

    #[cfg(not(windows))]
    fn assign(&self, child: &Child) -> Result<()> {
        *self.0.lock() = Some(child.id() as i32);
        Ok(())
    }

    fn terminate(&self) {
        #[cfg(windows)]
        if let Some(job) = self.handle() {
            let _ = unsafe { win::TerminateJobObject(job, 1) };
        }
        #[cfg(unix)]
        if let Some(pgid) = self.0.lock().take()
            && pgid > 0
        {
            unsafe {
                let _ = libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }

    fn process_ids(&self) -> Vec<u32> {
        #[cfg(windows)]
        if let Some(job) = self.handle() {
            #[repr(C)]
            struct IdList {
                assigned: u32,
                listed: u32,
                ids: [usize; 64],
            }
            let mut list = IdList { assigned: 0, listed: 0, ids: [0; 64] };
            let ok = unsafe {
                win::QueryInformationJobObject(
                    Some(job),
                    win::JobObjectBasicProcessIdList,
                    &mut list as *mut _ as *mut core::ffi::c_void,
                    std::mem::size_of::<IdList>() as u32,
                    None,
                )
            };
            if ok.is_ok() {
                return list.ids[..(list.listed as usize).min(64)].iter().map(|&id| id as u32).collect();
            }
        }
        #[cfg(unix)]
        if let Some(pgid) = *self.0.lock()
            && pgid > 0
            && super::process_is_running(pgid as u32)
        {
            return vec![pgid as u32];
        }
        Vec::new()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        #[cfg(windows)]
        if let Some(job) = self.handle() {
            // KILL_ON_JOB_CLOSE: closing the last handle kills whatever is left.
            let _ = unsafe { win::CloseHandle(job) };
        }
        #[cfg(not(windows))]
        self.terminate();
    }
}

/// Resume every thread of a process created with CREATE_SUSPENDED.
#[cfg(windows)]
fn resume_process(pid: u32) -> Result<()> {
    use win::*;
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0).context("CreateToolhelp32Snapshot")?;
        let mut entry = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
        let mut resumed = 0;
        let mut more = Thread32First(snap, &mut entry).is_ok();
        while more {
            if entry.th32OwnerProcessID == pid
                && let Ok(thread) = OpenThread(THREAD_SUSPEND_RESUME, false, entry.th32ThreadID)
            {
                if ResumeThread(thread) != u32::MAX {
                    resumed += 1;
                }
                let _ = CloseHandle(thread);
            }
            more = Thread32Next(snap, &mut entry).is_ok();
        }
        let _ = CloseHandle(snap);
        if resumed == 0 {
            bail!("could not resume the speech engine process");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn resume_process(_pid: u32) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Supervision against a fake sidecar (a few lines of Python speaking protocol v1), so restarts,
    //! back-off, the CPU fallback, the Job Object and timeouts are tested without loading a model.
    //! Needs the runtime interpreter; skipped (with a note) when it is not installed.
    use super::super::{Engine, EngineStatus, TranscribeRequest, process_is_running};
    use super::*;
    use crate::paths::Paths;
    use crate::settings::EngineSettings;
    use std::path::PathBuf;

    const FAKE: &str = r#"
import json, os, sys, time
args = sys.argv[1:]
model = args[args.index("--model") + 1]
cpu = "--device" in args and args[args.index("--device") + 1] == "cpu"
def send(m):
    m["v"] = 1
    sys.stdout.write(json.dumps(m) + "\n"); sys.stdout.flush()
if model == "crash-loading" and not cpu:
    sys.stderr.write("fake: crashing while loading\n"); os._exit(1)
time.sleep(0.2)
if model == "badbytes":
    sys.stdout.flush(); sys.stdout.buffer.write(b"\xff\xfe not utf-8\n"); sys.stdout.buffer.flush()
send({"type": "log", "level": "info", "msg": "fake loaded"})
send({"type": "ready", "engine": "faster-whisper", "model": model, "device": "cpu" if cpu else "cuda",
      "compute": "int8", "vram_mb": 7, "caps": {"hotwords": False, "prompt": True, "vad": True}})
if model == "die-after-ready":
    time.sleep(0.2); os._exit(1)
for line in sys.stdin:
    m = json.loads(line)
    if m["type"] == "ping":
        send({"type": "pong"})
    elif m["type"] == "shutdown":
        sys.exit(0)
    elif m["type"] == "transcribe":
        if model == "slow":
            continue
        name = os.path.basename(m["audio_path"])
        if name == "bad.wav":
            send({"type": "error", "id": m["id"], "code": "BAD_AUDIO", "msg": "audio file not found"})
        elif name == "oom.wav":
            send({"type": "error", "id": m["id"], "code": "OOM", "msg": "out of GPU memory"})
        else:
            send({"type": "result", "id": m["id"], "text": "heard " + name + " " + " ".join(m["bias_terms"]),
                  "speech_ms": 1000, "infer_ms": 5,
                  "segments": [{"start": 0.0, "end": 1.0, "text": "heard", "no_speech_prob": 0.1}]})
"#;

    fn runtime_python() -> Option<PathBuf> {
        #[cfg(windows)]
        let real = std::env::var_os("LOCALAPPDATA")
            .map(|l| PathBuf::from(l).join("Wren").join("runtime").join(".venv").join("Scripts").join("python.exe"));
        #[cfg(target_os = "macos")]
        let real = std::env::var_os("HOME").map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("Wren")
                .join("runtime")
                .join(".venv")
                .join("bin")
                .join("python")
        });
        #[cfg(not(any(windows, target_os = "macos")))]
        let real: Option<PathBuf> = None;
        [Some(Paths::resolve().runtime_python()), real].into_iter().flatten().find(|p| p.is_file())
    }

    struct Fake {
        dir: PathBuf,
        engine: Engine,
        status_rx: Receiver<EngineStatus>,
    }

    /// The fake needs the runtime interpreter. Missing runtime = a loud failure (never a silent green),
    /// unless WREN_ALLOW_NO_RUNTIME=1 says to skip these tests on purpose.
    fn fake(model: &str) -> Option<Fake> {
        let Some(python) = runtime_python() else {
            if std::env::var("WREN_ALLOW_NO_RUNTIME").as_deref() == Ok("1") {
                eprintln!("SKIPPED (WREN_ALLOW_NO_RUNTIME=1): supervisor test needs the runtime python");
                return None;
            }
            panic!("the speech runtime is not installed: run scripts/setup-runtime.ps1 (or set WREN_ALLOW_NO_RUNTIME=1 to skip on purpose)");
        };
        let dir = std::env::temp_dir().join(format!("wren-fake-sidecar-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("wren_sidecar")).unwrap();
        std::fs::write(dir.join("wren_sidecar").join("__init__.py"), "").unwrap();
        std::fs::write(dir.join("wren_sidecar").join("__main__.py"), FAKE).unwrap();
        let settings = EngineSettings { model: model.into(), ..EngineSettings::default() };
        let cfg = EngineConfig {
            python,
            sidecar_dir: dir.clone(),
            models_dir: dir.join("models"),
            warmup_wav: dir.join("warmup.wav"),
            log_file: dir.join("logs").join("sidecar.log"),
            settings,
        };
        let (tx, status_rx) = crossbeam_channel::unbounded();
        Some(Fake { dir, engine: Engine::start_with(cfg, tx), status_rx })
    }

    fn wait_for(rx: &Receiver<EngineStatus>, timeout: Duration, pred: impl Fn(&EngineStatus) -> bool) -> Option<EngineStatus> {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(s) if pred(&s) => return Some(s),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// The python pids among `pids`, launcher first.
    fn pythons(pids: &[u32]) -> Vec<u32> {
        pids.iter()
            .copied()
            .filter(|&p| {
                super::super::process_image(p).is_some_and(|i| {
                    i.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.eq_ignore_ascii_case("python.exe") || n.to_ascii_lowercase().starts_with("python"))
                })
            })
            .collect()
    }

    fn is_ready(s: &EngineStatus) -> bool {
        matches!(s, EngineStatus::Ready { .. })
    }

    fn req(id: &str, file: &str) -> TranscribeRequest {
        TranscribeRequest { id: id.into(), audio_path: PathBuf::from(file), bias_terms: vec!["Wren".into()], prompt: String::new() }
    }

    #[test]
    fn ready_transcribe_errors_ping_and_clean_shutdown() {
        let Some(f) = fake("ok") else { return };
        assert_eq!(f.engine.transcribe(req("early", "a.wav"), Duration::from_secs(1)), Err(EngineError::NotReady));
        let ready = wait_for(&f.status_rx, Duration::from_secs(15), is_ready).expect("ready");
        assert_eq!(ready, EngineStatus::Ready { model: "ok".into(), device: "cuda".into(), vram_mb: 7 });
        let sample_path = PathBuf::from("x").join("a.wav");
        let r = f.engine.transcribe(req("d1", &sample_path.to_string_lossy()), Duration::from_secs(5)).unwrap();
        assert_eq!((r.text.as_str(), r.speech_ms, r.infer_ms, r.segments.len()), ("heard a.wav Wren", 1000, 5, 1));
        assert!(matches!(f.engine.transcribe(req("d2", "bad.wav"), Duration::from_secs(5)), Err(EngineError::BadAudio(_))));
        assert!(matches!(f.engine.transcribe(req("d3", "oom.wav"), Duration::from_secs(5)), Err(EngineError::Oom(_))));
        assert!(f.engine.ping(Duration::from_secs(5)));

        let pids = f.engine.process_ids();
        #[cfg(windows)]
        {
            assert!(pids.len() >= 2, "venv launcher + interpreter (+ conhost), all in the job: {pids:?}");
            assert_eq!(pythons(&pids).len(), 2, "launcher and interpreter");
        }
        #[cfg(not(windows))]
        {
            assert!(!pids.is_empty(), "sidecar pid tracked: {pids:?}");
            assert!(!pythons(&pids).is_empty(), "python process tracked: {pids:?}");
        }
        assert!(pids.iter().all(|&p| process_is_running(p)));
        let log = std::fs::read_to_string(f.dir.join("logs").join("sidecar.log")).unwrap();
        assert!(log.contains("--- sidecar launch"));
        drop(f.engine);
        assert!(pids.iter().all(|&p| !process_is_running(p)), "no sidecar process outlives the engine");
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn killed_interpreter_is_restarted_after_backoff() {
        let Some(f) = fake("ok") else { return };
        wait_for(&f.status_rx, Duration::from_secs(15), is_ready).expect("ready");
        let pids = f.engine.process_ids();
        let py_pids = pythons(&pids);
        let interpreter = *py_pids.last().expect("python pid");
        let t0 = Instant::now();
        #[cfg(windows)]
        {
            let out = std::process::Command::new("taskkill").args(["/F", "/PID", &interpreter.to_string()]).output().unwrap();
            assert!(out.status.success());
        }
        #[cfg(unix)]
        {
            assert_eq!(unsafe { libc::kill(interpreter as libc::pid_t, libc::SIGKILL) }, 0);
        }
        // Review r1: the crash must show at once, not only after the back-off.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!is_ready(&f.engine.status()), "status still Ready 200 ms after the kill: {:?}", f.engine.status());
        assert_eq!(f.engine.transcribe(req("dead", "a.wav"), Duration::from_secs(1)), Err(EngineError::NotReady));
        wait_for(&f.status_rx, Duration::from_secs(5), |s| *s == EngineStatus::Starting).expect("noticed the crash");
        assert!(t0.elapsed() < Duration::from_millis(SIDECAR_RESTART_BACKOFF_MS[0]), "Starting was sent before the back-off, not after");
        wait_for(&f.status_rx, Duration::from_secs(10), is_ready).expect("ready again");
        let took = t0.elapsed();
        assert!(took >= Duration::from_millis(SIDECAR_RESTART_BACKOFF_MS[0]), "waited the first back-off step: {took:?}");
        assert!(pids.iter().all(|&p| !process_is_running(p)), "old launcher went too");
        let r = f.engine.transcribe(req("after", "a.wav"), Duration::from_secs(5)).unwrap();
        assert!(r.text.starts_with("heard"));
        drop(f.engine);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn unanswered_request_times_out_and_restart_recovers() {
        let Some(f) = fake("slow") else { return };
        wait_for(&f.status_rx, Duration::from_secs(15), is_ready).expect("ready");
        let t0 = Instant::now();
        assert_eq!(f.engine.transcribe(req("t1", "a.wav"), Duration::from_millis(300)), Err(EngineError::Timeout));
        assert!(t0.elapsed() < Duration::from_secs(2));
        let before = f.engine.process_ids();
        f.engine.restart();
        wait_for(&f.status_rx, Duration::from_secs(5), |s| *s == EngineStatus::Starting).expect("restarting");
        wait_for(&f.status_rx, Duration::from_secs(15), is_ready).expect("ready after restart");
        assert!(before.iter().all(|&p| !process_is_running(p)));
        drop(f.engine);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn non_utf8_output_is_skipped_not_fatal() {
        let Some(f) = fake("badbytes") else { return };
        wait_for(&f.status_rx, Duration::from_secs(15), is_ready).expect("ready despite a non-UTF-8 line");
        let pids = f.engine.process_ids();
        let r = f.engine.transcribe(req("u1", "a.wav"), Duration::from_secs(5)).unwrap();
        assert!(r.text.starts_with("heard"));
        assert_eq!(f.engine.process_ids(), pids, "same sidecar, no restart");
        drop(f.engine);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn crashes_while_loading_fall_back_to_cpu() {
        let Some(f) = fake("crash-loading") else { return };
        let ready = wait_for(&f.status_rx, Duration::from_secs(20), is_ready).expect("ready on cpu");
        assert_eq!(ready, EngineStatus::Ready { model: "crash-loading".into(), device: "cpu".into(), vram_mb: 7 });
        drop(f.engine);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn repeated_failures_give_up_until_restart() {
        let Some(f) = fake("die-after-ready") else { return };
        // 5 failures: back-off 1 + 2 + 5 + 5 s between them.
        let failed = wait_for(&f.status_rx, Duration::from_secs(40), |s| matches!(s, EngineStatus::Failed { .. }))
            .expect("gave up");
        let EngineStatus::Failed { reason } = failed else { unreachable!() };
        assert!(reason.contains("5 times in 5 minutes") && reason.contains("Restart it from Settings"), "{reason}");
        assert!(matches!(f.engine.status(), EngineStatus::Failed { .. }));
        f.engine.restart();
        wait_for(&f.status_rx, Duration::from_secs(10), is_ready).expect("tries again after restart()");
        drop(f.engine);
        let _ = std::fs::remove_dir_all(&f.dir);
    }

    #[test]
    fn missing_runtime_is_reported_not_panicked() {
        let dir = std::env::temp_dir().join(format!("wren-no-runtime-{}", uuid::Uuid::new_v4()));
        let cfg = EngineConfig::from_paths(&Paths::under(&dir), &EngineSettings::default());
        let (tx, rx) = crossbeam_channel::unbounded();
        let engine = Engine::start_with(cfg, tx);
        // No runtime: it waits for setup (no launch, no crash loop), never Ready.
        let waiting = wait_for(&rx, Duration::from_millis(1_500), |s| matches!(s, EngineStatus::NeedsSetup { .. }));
        assert!(matches!(waiting, Some(EngineStatus::NeedsSetup { reason }) if reason.contains("isn't set up")));
        assert!(wait_for(&rx, Duration::from_millis(500), is_ready).is_none());
        assert_eq!(engine.transcribe(req("x", "a.wav"), Duration::from_secs(1)), Err(EngineError::NotReady));
        // A restart without the files still just waits (setup has not run).
        engine.restart();
        assert!(matches!(
            wait_for(&rx, Duration::from_millis(1_500), |s| matches!(s, EngineStatus::NeedsSetup { .. })),
            Some(EngineStatus::NeedsSetup { .. })
        ) || matches!(engine.status(), EngineStatus::NeedsSetup { .. }));
        let t0 = Instant::now();
        drop(engine);
        assert!(t0.elapsed() < Duration::from_secs(2), "shutdown does not wait");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
