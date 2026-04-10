use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use anyhow::Result;
use chaos_proc::runtime_db_path;
use rama::graceful;
use rama::http::server::HttpServer;
use rama::rt::Executor;
use tracing::error;
use tracing::info;

use crate::JournalRpcServer;
use crate::SqliteJournalStore;

const DEFAULT_SOCKET_FILENAME: &str = "journald.sock";

#[derive(Debug, Clone)]
pub struct JournalServerConfig {
    pub socket_path: PathBuf,
    pub sqlite_db_path: PathBuf,
}

impl JournalServerConfig {
    pub fn from_chaos_home(chaos_home: &Path) -> Self {
        Self {
            socket_path: default_socket_path_in(),
            sqlite_db_path: sqlite_db_path_in(chaos_home),
        }
    }

    pub fn discover() -> std::io::Result<Self> {
        let chaos_home = chaos_pwd::find_chaos_home()?;
        Ok(Self::from_chaos_home(chaos_home.as_path()))
    }
}

pub fn default_socket_path() -> std::io::Result<PathBuf> {
    Ok(default_socket_path_in())
}

pub fn default_socket_runtime_dir() -> std::io::Result<PathBuf> {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(runtime_dir).join("chaos"));
    }

    let uid = unsafe { libc::geteuid() };
    Ok(std::env::temp_dir().join(format!("chaos-{uid}")))
}

pub fn sqlite_db_path() -> std::io::Result<PathBuf> {
    let chaos_home = chaos_pwd::find_chaos_home()?;
    Ok(sqlite_db_path_in(chaos_home.as_path()))
}

pub async fn run_sqlite_journal_server(config: JournalServerConfig) -> Result<()> {
    let socket_parent = config
        .socket_path
        .parent()
        .context("journal socket path missing parent")?;
    tokio::fs::create_dir_all(socket_parent)
        .await
        .with_context(|| format!("create journal run dir {}", socket_parent.display()))?;
    ensure_runtime_dir_permissions(socket_parent).await?;
    remove_stale_socket(&config.socket_path).await?;

    let db_parent = config
        .sqlite_db_path
        .parent()
        .context("journal sqlite path missing parent")?;
    tokio::fs::create_dir_all(db_parent)
        .await
        .with_context(|| format!("create journal db dir {}", db_parent.display()))?;

    let socket_guard = SocketPathGuard::new(config.socket_path.clone());
    let store = Arc::new(
        SqliteJournalStore::open(&config.sqlite_db_path)
            .await
            .with_context(|| {
                format!("open sqlite journal db {}", config.sqlite_db_path.display())
            })?,
    );

    let graceful = graceful::Shutdown::new(async move {
        let mut signal = Box::pin(graceful::default_signal());
        signal.as_mut().await;
    });
    let exec = Executor::graceful(graceful.guard());
    let service = JournalRpcServer::new(store, "sqlite").http_service();
    let socket_path = config.socket_path.clone();

    let mut serve_task = tokio::spawn(async move {
        info!(
            socket_path = %socket_path.display(),
            db_path = %config.sqlite_db_path.display(),
            rpc_path = crate::JOURNAL_RPC_PATH,
            "chaos-journald listening"
        );

        HttpServer::new_http1(exec)
            .listen_unix(&socket_path, service)
            .await
    });

    tokio::select! {
        result = &mut serve_task => {
            return match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(anyhow::anyhow!("serve unix journal RPC: {err}")),
                Err(err) => Err(err).context("join journal server task"),
            };
        }
        shutdown_delay = graceful.shutdown() => {
            info!(
                socket_path = %socket_guard.path().display(),
                shutdown_delay_ms = shutdown_delay.as_millis(),
                "chaos-journald shutting down"
            );
        }
    }

    match serve_task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(anyhow::anyhow!("serve unix journal RPC: {err}")),
        Err(err) => Err(err).context("join journal server task"),
    }
}

fn default_socket_path_in() -> PathBuf {
    default_socket_runtime_dir()
        .unwrap_or_else(|_| std::env::temp_dir().join("chaos"))
        .join(DEFAULT_SOCKET_FILENAME)
}

fn sqlite_db_path_in(chaos_home: &Path) -> PathBuf {
    runtime_db_path(chaos_home)
}

async fn remove_stale_socket(path: &Path) -> Result<()> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove stale socket {}", path.display())),
    }
}

pub(crate) async fn ensure_runtime_dir_permissions(path: &Path) -> Result<()> {
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .with_context(|| format!("set runtime dir permissions {}", path.display()))?;
    Ok(())
}

#[derive(Debug)]
struct SocketPathGuard {
    path: PathBuf,
}

impl SocketPathGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn path(&self) -> &Path {
        self.path.as_path()
    }
}

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != ErrorKind::NotFound
        {
            error!(
                "failed removing journal socket {}: {err}",
                self.path.display()
            );
        }
    }
}
