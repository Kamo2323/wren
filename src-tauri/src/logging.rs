//! JSON-lines logs in `%LOCALAPPDATA%\Wren\logs`, rotated daily.
//! Rule: logs never contain dictated text (it would outlive the 3-day deletion).

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Start logging. Keep the returned guard alive for the life of the app or buffered lines are lost.
/// If the log folder cannot be used, logging falls back to stderr only — never a panic.
pub fn init(logs_dir: &Path) -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_env("WREN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("wren")
        .filename_suffix("log")
        .max_log_files(MAX_LOG_FILES)
        .build(logs_dir);
    match appender {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let file_layer = fmt::layer().json().with_writer(writer).with_current_span(false);
            let ok = tracing_subscriber::registry()
                .with(filter)
                .with(file_layer)
                .with(fmt::layer().with_writer(std::io::stderr).with_target(false))
                .try_init()
                .is_ok();
            ok.then_some(guard) // Err = already initialised (tests)
        }
        Err(err) => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(fmt::layer().with_writer(std::io::stderr).with_target(false))
                .try_init();
            tracing::error!(%err, "log folder unusable; logging to stderr only");
            None
        }
    }
}

/// Days of logs kept.
const MAX_LOG_FILES: usize = 14;

/// Log panics (location only — the message could contain dictated text) before the process aborts.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".into());
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        tracing::error!(%location, %thread, "panic");
        eprintln!("Wren panicked at {location} on thread {thread}");
    }));
}
