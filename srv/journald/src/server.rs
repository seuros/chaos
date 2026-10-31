use chaos_snitch::tracing;
use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use anyhow::Result;
use chaos_proc::runtime_db_path;
use rama::graceful;
use rama::http::server::HttpServer;
use rama::rt::Executor;
use rama::unix::server::UnixListener;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::JournalRpcServer;
use crate::SqliteJournalStore;

const DEFAULT_SOCKET_FILENAME: &str = "journald.sock";
const SOCKET_WATCH_INTERVAL: Duration = Duration::from_secs(2);

type SocketIdentity = (u64, u64);

#[derive(Debug, Clone)]
pub struct JournalServerConfig {
    pub socket_path: PathBuf,
    pub sqlite_db_path: PathBuf,
}

impl JournalServerConfig {
    pub fn from_chaos_home(chaos_home: &Path) -> Self {
        Self {
            socket_path: default_socket_path_in(chaos_home),
            sqlite_db_path: sqlite_db_path_in(chaos_home),
        }
    }

    pub fn discover() -> std::io::Result<Self> {
        let chaos_home = chaos_pwd::find_chaos_home()?;
        Ok(Self::from_chaos_home(chaos_home.as_path()))
    }
}

pub fn default_socket_path() -> std::io::Result<PathBuf> {
    let chaos_home = chaos_pwd::find_chaos_home()?;
    Ok(default_socket_path_in(chaos_home.as_path()))
}

/// Runtime directory beside a SQLite database, holding its journald socket
/// and startup lock, so each database is served by its own daemon.
pub fn default_socket_runtime_dir(chaos_home: &Path) -> PathBuf {
    chaos_home.join("run")
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

    let store = Arc::new(
        SqliteJournalStore::open(&config.sqlite_db_path)
            .await
            .with_context(|| {
                format!("open sqlite journal db {}", config.sqlite_db_path.display())
            })?,
    );

    // Bind without rama's path cleanup: a displaced server must not unlink the
    // socket of the journald that replaced it.
    let listener = std::os::unix::net::UnixListener::bind(&config.socket_path)
        .with_context(|| format!("bind journal socket {}", config.socket_path.display()))?;
    let identity = socket_identity(&config.socket_path)
        .await?
        .context("journal socket vanished after bind")?;
    let socket_guard = SocketPathGuard {
        path: config.socket_path.clone(),
        identity,
    };

    let watched = config.socket_path.clone();
    let graceful = graceful::Shutdown::new(async move {
        tokio::select! {
            () = graceful::default_signal() => {}
            () = socket_released(watched, identity) => {}
        }
    });
    let exec = Executor::graceful(graceful.guard());
    let service = HttpServer::new_http1(exec.clone())
        .service(JournalRpcServer::new(store, "sqlite").http_service());
    let listener =
        UnixListener::try_from_std_unix_listener(listener, exec).context("serve journal socket")?;

    info!(
        socket_path = %config.socket_path.display(),
        db_path = %config.sqlite_db_path.display(),
        rpc_path = crate::JOURNAL_RPC_PATH,
        "chaos_journald listening"
    );
    let mut serve_task = tokio::spawn(listener.serve(service));

    tokio::select! {
        result = &mut serve_task => {
            return result.context("join journal server task");
        }
        shutdown_delay = graceful.shutdown() => {
            info!(
                socket_path = %socket_guard.path.display(),
                shutdown_delay_ms = shutdown_delay.as_millis(),
                "chaos_journald shutting down"
            );
        }
    }

    serve_task.await.context("join journal server task")
}

fn default_socket_path_in(chaos_home: &Path) -> PathBuf {
    default_socket_runtime_dir(chaos_home).join(DEFAULT_SOCKET_FILENAME)
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

/// Resolves once `path` stops naming the socket this server bound: a newer
/// journald replaced it or its home was deleted, so no client can reach us.
async fn socket_released(path: PathBuf, identity: SocketIdentity) {
    loop {
        tokio::time::sleep(SOCKET_WATCH_INTERVAL).await;
        match socket_identity(&path).await {
            Ok(current) if current == Some(identity) => {}
            Ok(_) => {
                info!(
                    socket_path = %path.display(),
                    "journal socket was removed or replaced"
                );
                return;
            }
            Err(err) => warn!("failed checking journal socket {}: {err}", path.display()),
        }
    }
}

async fn socket_identity(path: &Path) -> std::io::Result<Option<SocketIdentity>> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) if meta.file_type().is_socket() => Ok(Some((meta.dev(), meta.ino()))),
        Ok(_) => Ok(None),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[derive(Debug)]
struct SocketPathGuard {
    path: PathBuf,
    identity: SocketIdentity,
}

impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        let owned = std::fs::symlink_metadata(&self.path)
            .is_ok_and(|meta| (meta.dev(), meta.ino()) == self.identity);
        if owned
            && let Err(err) = std::fs::remove_file(&self.path)
            && err.kind() != ErrorKind::NotFound
        {
            error!(
                "failed removing journal socket {}: {err}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests;
