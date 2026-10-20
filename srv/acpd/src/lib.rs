//! Native Chaos processes exposed through the reusable ACP host.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use acp_host::{AgentBackend, AgentServer, AgentSession, protocol};
use chaos_argv::Arg0DispatchPaths;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::config::{Config, ConfigBuilder, ConfigOverrides};
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_kern::{AuthManager, ProcessTable};
use protocol::schema::v1::NewSessionRequest;

mod approvals;
mod config;
mod projection;
mod session;

#[cfg(test)]
mod tests;

fn invalid(message: &'static str) -> protocol::Error {
    protocol::Error::invalid_params().data(message)
}

fn internal(message: &'static str) -> protocol::Error {
    protocol::Error::internal_error().data(message)
}

#[derive(Clone)]
enum ConfigSource {
    Fixed(Arc<Config>),
    Project {
        home: PathBuf,
        overrides: Vec<(String, toml::Value)>,
        profile: Option<String>,
        alcatraz: PathBuf,
    },
}

impl ConfigSource {
    async fn load(&self, cwd: &std::path::Path) -> protocol::Result<Config> {
        match self {
            Self::Fixed(config) => {
                if cwd != config.cwd {
                    return Err(invalid(
                        "ACP cwd differs from the embedded trusted workspace",
                    ));
                }
                Ok(config.as_ref().clone())
            }
            Self::Project {
                home,
                overrides,
                profile,
                alcatraz,
            } => ConfigBuilder::default()
                .chaos_home(home.clone())
                .cli_overrides(overrides.clone())
                .harness_overrides(ConfigOverrides {
                    cwd: Some(cwd.to_path_buf()),
                    config_profile: profile.clone(),
                    alcatraz_exe: Some(alcatraz.clone()),
                    ..Default::default()
                })
                .build()
                .await
                .map_err(|_| internal("Chaos project configuration could not be loaded")),
        }
    }
}

struct Backend(ConfigSource);

impl AgentBackend for Backend {
    fn new_session(
        &self,
        request: NewSessionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<Arc<dyn AgentSession>>> {
        Box::pin(async move {
            if !request.cwd.is_absolute()
                || !tokio::fs::metadata(&request.cwd)
                    .await
                    .is_ok_and(|entry| entry.is_dir())
            {
                return Err(invalid("ACP cwd must be an existing absolute directory"));
            }
            let config = self.0.load(&request.cwd).await?;
            config::validate_mcp(&config, &request.mcp_servers)?;
            let auth = AuthManager::shared(
                config.chaos_home.clone(),
                false,
                config.cli_auth_credentials_store_mode,
            );
            let table = Arc::new(ProcessTable::new(
                &config,
                auth,
                SessionSource::Api,
                CollaborationModesConfig {
                    default_mode_request_user_input: false,
                },
            ));
            // Installed before the first spawn await. A cancelled session/new
            // cannot abandon a kernel process that never reached ACP ownership.
            let guard = session::OwnerGuard::new(table.clone());
            let created = table.start_process(config).await.map_err(|_| {
                internal("Chaos session startup failed; check provider configuration")
            })?;
            let (id, process, _) = created.into_parts();
            Ok(Arc::new(session::NativeSession::new(process, id, guard)) as Arc<dyn AgentSession>)
        })
    }
}

fn server(source: ConfigSource) -> io::Result<AgentServer> {
    AgentServer::builder(Backend(source))
        .identity("chaos", CHAOS_VERSION)
        .build()
        .map_err(io::Error::other)
}

/// Embed an ACP endpoint for a fixed, explicitly configured trusted workspace.
pub async fn build_server(config: Config) -> io::Result<AgentServer> {
    chaos_kern::runtime_db::mount_vfs_for_startup(&config)
        .await
        .map_err(io::Error::other)?;
    server(ConfigSource::Fixed(Arc::new(config)))
}

/// Serve stable ACP v1 on stdio; all logging stays on stderr.
pub async fn run_main(
    paths: Arg0DispatchPaths,
    overrides: CliConfigOverrides,
    profile: Option<String>,
) -> io::Result<()> {
    let overrides = overrides.parse_overrides().map_err(io::Error::other)?;
    let config = ConfigBuilder::default()
        .cli_overrides(overrides.clone())
        .harness_overrides(ConfigOverrides {
            config_profile: profile.clone(),
            alcatraz_exe: Some(paths.alcatraz_exe.clone()),
            ..Default::default()
        })
        .build()
        .await?;
    chaos_kern::runtime_db::mount_vfs_for_startup(&config)
        .await
        .map_err(io::Error::other)?;
    let _ = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let host = server(ConfigSource::Project {
        home: config.chaos_home,
        overrides,
        profile,
        alcatraz: paths.alcatraz_exe,
    })?;
    host.serve(tokio::io::stdin(), tokio::io::stdout())
        .await
        .map_err(io::Error::other)
}
