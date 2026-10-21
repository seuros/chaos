//! Native Chaos processes exposed through the reusable ACP host.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use acp_host::{AgentServer, protocol};
use chaos_argv::Arg0DispatchPaths;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_kern::config::{Config, ConfigBuilder, ConfigOverrides};

mod approvals;
mod backend;
mod commands;
mod config;
mod controls;
mod elicitation;
mod files;
mod history;
mod projection;
mod replay;
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
        base: Arc<Config>,
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
                let cwd = tokio::fs::canonicalize(cwd)
                    .await
                    .map_err(|_| invalid("ACP cwd is unavailable"))?;
                let configured = tokio::fs::canonicalize(&config.cwd)
                    .await
                    .map_err(|_| invalid("trusted ACP workspace is unavailable"))?;
                if cwd != configured {
                    return Err(invalid(
                        "ACP cwd differs from the embedded trusted workspace",
                    ));
                }
                let mut config = config.as_ref().clone();
                config.cwd = cwd;
                Ok(config)
            }
            Self::Project {
                home,
                overrides,
                profile,
                alcatraz,
                base,
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
                .map_err(|_| internal("Chaos project configuration could not be loaded"))
                .and_then(|config| {
                    if config.model_provider_id != base.model_provider_id {
                        Err(invalid(
                            "ACP project configuration changes the selected provider",
                        ))
                    } else {
                        Ok(config)
                    }
                }),
        }
    }
    fn base(&self) -> &Config {
        match self {
            Self::Fixed(config) | Self::Project { base: config, .. } => config,
        }
    }
}

struct Backend(ConfigSource);

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
        base: Arc::new(config.clone()),
        home: config.chaos_home,
        overrides,
        profile,
        alcatraz: paths.alcatraz_exe,
    })?;
    host.serve(tokio::io::stdin(), tokio::io::stdout())
        .await
        .map_err(io::Error::other)
}
