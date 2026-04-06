use std::fs::File;
use std::fs::OpenOptions;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

use anyhow::Context as _;
use anyhow::Result;

pub const DEFAULT_BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_FILENAME: &str = "journald.lock";
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone)]
pub struct BootstrapPaths {
    pub socket_path: PathBuf,
    pub sqlite_db_path: PathBuf,
    pub runtime_dir: PathBuf,
}

impl BootstrapPaths {
    pub fn discover() -> std::io::Result<Self> {
        let config = crate::JournalServerConfig::discover()?;
        Ok(Self {
            runtime_dir: crate::default_socket_runtime_dir()?,
            socket_path: config.socket_path,
            sqlite_db_path: config.sqlite_db_path,
        })
    }

    pub fn lock_path(&self) -> PathBuf {
        self.runtime_dir.join(LOCK_FILENAME)
    }
}

pub fn runtime_socket_dir() -> std::io::Result<PathBuf> {
    crate::default_socket_runtime_dir()
}

pub async fn ensure_sqlite_journald_running(binary_path: Option<&Path>) -> Result<BootstrapPaths> {
    let paths = BootstrapPaths::discover()?;
    tokio::fs::create_dir_all(&paths.runtime_dir)
        .await
        .with_context(|| format!("create journal runtime dir {}", paths.runtime_dir.display()))?;
    crate::server::ensure_runtime_dir_permissions(paths.runtime_dir.as_path()).await?;

    if server_is_compatible(paths.socket_path.as_path()).await {
        return Ok(paths);
    }

    let lock_file = acquire_startup_lock(paths.lock_path().as_path()).await?;
    if server_is_compatible(paths.socket_path.as_path()).await {
        drop(lock_file);
        return Ok(paths);
    }

    let executable = resolve_journald_executable(binary_path)?;
    if socket_exists(paths.socket_path.as_path()).await {
        tokio::fs::remove_file(paths.socket_path.as_path())
            .await
            .with_context(|| {
                format!(
                    "remove incompatible journal socket {}",
                    paths.socket_path.display()
                )
            })?;
    }
    spawn_detached_journald(executable.as_path(), &paths)
        .with_context(|| format!("spawn detached journald {}", executable.display()))?;
    wait_for_compatible_server(paths.socket_path.as_path(), DEFAULT_BOOTSTRAP_TIMEOUT).await?;
    drop(lock_file);
    Ok(paths)
}

async fn acquire_startup_lock(lock_path: &Path) -> Result<File> {
    let lock_path = lock_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("open startup lock {}", lock_path.display()))?;
        file.lock()
            .with_context(|| format!("lock startup file {}", lock_path.display()))?;
        Ok::<_, anyhow::Error>(file)
    })
    .await
    .context("join startup lock task")?
}

fn resolve_journald_executable(binary_path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = binary_path {
        return Ok(path.to_path_buf());
    }

    // Check CARGO_BIN_EXE_* env vars set by cargo test / nextest.
    for key in [
        "CARGO_BIN_EXE_chaos-journald",
        "CARGO_BIN_EXE_chaos_journald",
    ] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.exists() {
                return Ok(path);
            }
        }
    }

    let current_exe = std::env::current_exe().context("resolve current executable")?;
    if let Some(parent) = current_exe.parent() {
        let sibling = parent.join("chaos-journald");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    Ok(PathBuf::from("chaos-journald"))
}

fn spawn_detached_journald(binary_path: &Path, paths: &BootstrapPaths) -> Result<()> {
    let mut command = std::process::Command::new(binary_path);
    command
        .arg("--socket")
        .arg(&paths.socket_path)
        .arg("--db")
        .arg(&paths.sqlite_db_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    command.spawn().context("spawn journald child")?;
    Ok(())
}

async fn wait_for_compatible_server(socket_path: &Path, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        if server_is_compatible(socket_path).await {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "timed out waiting for compatible journal server on {}",
                socket_path.display()
            );
        }
        tokio::time::sleep(STARTUP_POLL_INTERVAL).await;
    }
}

async fn socket_exists(socket_path: &Path) -> bool {
    tokio::fs::metadata(socket_path)
        .await
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

async fn server_is_compatible(socket_path: &Path) -> bool {
    if !socket_exists(socket_path).await {
        return false;
    }

    let client = crate::JournalRpcClient::new(socket_path.to_path_buf());
    match client.hello("bootstrap").await {
        Ok(hello) => hello.protocol_version >= crate::rama_http::PROTOCOL_VERSION,
        Err(_) => false,
    }
}
