//! JSON-lines logs in `%LOCALAPPDATA%\Wren\logs`, rotated daily.
//! Rule: logs never contain dictated text (it would outlive the 3-day deletion).

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Start logging. Keep the returned guard alive for the life of the app or buffered lines are lost.
pub fn init(logs_dir: &Path) -> Option<WorkerGuard> {
    let appender = tracing_appender::rolling::daily(logs_dir, "wren.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("WREN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = fmt::layer().json().with_writer(writer).with_current_span(false);
    let result = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(fmt::layer().with_writer(std::io::stderr).with_target(false))
        .try_init();
    match result {
        Ok(()) => Some(guard),
        Err(_) => None, // already initialised (tests)
    }
}
