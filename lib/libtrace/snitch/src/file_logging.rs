//! Secure file-backed tracing layers.
//!
//! Provides helpers that open a log file with mode 0o600 and return a
//! [`tracing_subscriber`] [`Layer`] backed by a non-blocking writer.
//! Callers compose the returned layer into their own subscriber registry
//! and hold the [`WorkerGuard`] for the lifetime of the process.

use std::path::Path;

use tracing_appender::non_blocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::registry::LookupSpan;

/// The env-var name both bins consult for an optional secondary debug log.
const DEBUG_LOG_PATH_ENV_VAR: &str = "CHAOS_DEBUG_LOG_PATH";

/// Boxed [`Layer`] paired with its [`WorkerGuard`].
pub type BoxedLogLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// Open `path` with mode 0o600 (create + append) and return a non-blocking
/// [`fmt`] layer together with its [`WorkerGuard`].
///
/// `default_filter` is used when `RUST_LOG` is not set.  Pass a
/// `tracing_subscriber::fmt::format::FmtSpan` flags value via `span_events`
/// to record span open/close events; pass `FmtSpan::NONE` to omit them.
///
/// # Errors
///
/// Returns `Err` if the file cannot be opened.
pub fn open_log_file_layer<S>(
    path: &Path,
    default_filter: &str,
    span_events: FmtSpan,
) -> std::io::Result<(BoxedLogLayer<S>, WorkerGuard)>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let file = open_secure_file(path)?;
    let (non_blocking_writer, guard) = non_blocking(file);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_writer)
        .with_target(true)
        .with_ansi(false)
        .with_span_events(span_events)
        .with_filter(filter)
        .boxed();
    Ok((layer, guard))
}

/// Read `CHAOS_DEBUG_LOG_PATH` from the environment; if set, open the path
/// with mode 0o600 and return a non-blocking debug [`fmt`] layer plus its
/// [`WorkerGuard`].  Returns `(None, None)` when the variable is unset.
///
/// `default_filter` is used when `RUST_LOG` is not set.
///
/// # Errors
///
/// Returns `Err` if the variable is set but the file cannot be opened.
pub fn open_debug_log_file_layer<S>(
    default_filter: &str,
) -> std::io::Result<(Option<BoxedLogLayer<S>>, Option<WorkerGuard>)>
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let Some(path) = std::env::var_os(DEBUG_LOG_PATH_ENV_VAR).map(std::path::PathBuf::from) else {
        return Ok((None, None));
    };

    let file = open_secure_file(&path)?;
    let (non_blocking_writer, guard) = non_blocking(file);
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter));
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking_writer)
        .with_target(true)
        .with_ansi(false)
        .with_span_events(FmtSpan::NONE)
        .with_filter(filter)
        .boxed();
    Ok((Some(layer), Some(guard)))
}

/// Open a file at `path` for create+append with Unix mode 0o600.
fn open_secure_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }

    opts.open(path)
}
