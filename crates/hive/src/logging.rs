//! Logging initialization — tracing-subscriber for CLI, tracing-appender for daemon.

use std::path::Path;

/// Initialize stderr logging for CLI subcommands (INFO default, RUST_LOG override).
/// Call once at the start of main() for all non-daemon subcommands.
pub fn init_stderr() {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(true)
        .init();
}

/// Initialize file logging for daemon foreground mode.
/// Writes to `log_path` with timestamps, no ANSI.
/// CRITICAL: store the returned WorkerGuard for the entire process lifetime.
/// Dropping it shuts down the background flush thread and silently loses logs.
pub fn init_file(log_path: &Path) -> tracing_appender::non_blocking::WorkerGuard {
    use tracing_subscriber::{EnvFilter, fmt};
    let dir = log_path.parent().unwrap_or(Path::new("."));
    let name = log_path
        .file_name()
        .unwrap_or(std::ffi::OsStr::new("daemon.log"));
    std::fs::create_dir_all(dir).ok();
    let appender = tracing_appender::rolling::never(dir, name);
    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt()
        .with_env_filter(filter)
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(false)
        .init();
    guard
}
