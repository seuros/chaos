//! Secure file-backed tracing layers.
//!
//! Provides helpers that open a log file with mode 0o600 and return a
//! [`tracing_subscriber`] [`Layer`] backed by a non-blocking writer.
//! Callers compose the returned layer into their own subscriber registry
//! and hold the [`WorkerGuard`] for the lifetime of the process.

use std::fs::File;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use crate::tracing;
use crate::tracing::appender::non_blocking;
use crate::tracing::appender::non_blocking::WorkerGuard;
use crate::tracing::subscriber as tracing_subscriber;
use crate::tracing::subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::registry::LookupSpan;

/// The env-var name both bins consult for an optional secondary debug log.
const DEBUG_LOG_PATH_ENV_VAR: &str = "CHAOS_DEBUG_LOG_PATH";

const MAX_LOG_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Boxed [`Layer`] paired with its [`WorkerGuard`].
pub type BoxedLogLayer<S> = Box<dyn Layer<S> + Send + Sync + 'static>;

/// Open `path` with mode 0o600 (create + append) and return a non-blocking
/// [`fmt`] layer together with its [`WorkerGuard`]. Once the file passes
/// 10 MiB it is moved to `<path>.1`, replacing the previous backup.
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
    let file = RotatingFile::open(path, MAX_LOG_FILE_BYTES)?;
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

/// Append-only log file that moves itself to `<path>.1` before a write would
/// take it past `max_bytes`. Rotation is best-effort: if the rename or reopen
/// fails, writes keep going to the current file.
struct RotatingFile {
    path: PathBuf,
    backup_path: PathBuf,
    file: File,
    written: u64,
    max_bytes: u64,
}

impl RotatingFile {
    fn open(path: &Path, max_bytes: u64) -> io::Result<Self> {
        let file = open_secure_file(path)?;
        let written = file.metadata()?.len();
        let mut backup_path = path.as_os_str().to_owned();
        backup_path.push(".1");
        Ok(Self {
            path: path.to_path_buf(),
            backup_path: backup_path.into(),
            file,
            written,
            max_bytes,
        })
    }

    fn rotate(&mut self) {
        self.written = 0;
        match std::fs::rename(&self.path, &self.backup_path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return,
        }
        if let Ok(file) = open_secure_file(&self.path) {
            self.file = file;
        }
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.max_bytes {
            self.rotate();
        }
        let written = self.file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
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

#[cfg(test)]
mod tests;
