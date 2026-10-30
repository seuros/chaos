//! chaos-httpd — HTTP trigger server for Chaos.
#![deny(clippy::print_stdout, clippy::print_stderr)]
// sqlx 0.9's sqlite worker future, nested inside rama's service stack, pushes
// Send auto-trait evaluation past the default recursion depth.
#![recursion_limit = "256"]

extern crate usage as usage_rs;

mod api;
mod auth;
pub mod cli;
mod ingress;
#[cfg(feature = "mcpd")]
mod mcp;
mod monitor;
pub mod protocol;
mod runner;
mod sessions;
#[cfg(test)]
mod test_support;
mod webhooks;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use chaos_argv::Arg0DispatchPaths;
use chaos_coreboot::CoreBoot;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::config_types::SandboxMode;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::AuthManager;
use chaos_kern::ProcessTable;
use chaos_kern::config::{Config, ConfigBuilder, ConfigOverrides};
use chaos_kern::models_manager::CollaborationModesConfig;
use chaos_snitch::tracing::info;
use chaos_snitch::tracing::subscriber as tracing_subscriber;
use chaos_snitch::tracing::subscriber::EnvFilter;
use rama::graceful;
use rama::http::server::HttpServer;
use rama::rt::Executor;
use rama::tcp::server::TcpListener;
use tokio::sync::Semaphore;
use tracing_subscriber::prelude::*;

pub use cli::ServeCli;

const OTEL_SERVICE_NAME: &str = "chaos_httpd";
const DEFAULT_ANALYTICS_ENABLED: bool = true;

/// Shared state available to all HTTP handlers.
pub(crate) struct ServerState {
    config: Arc<Config>,
    process_table: Arc<ProcessTable>,
    #[allow(dead_code)]
    auth_manager: Arc<AuthManager>,
    bearer_token: Arc<str>,
    recovery_token: Option<Arc<str>>,
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    timeout: Duration,
    body_limit: usize,
    monitor: monitor::MonitorState,
    sessions: Option<Arc<sessions::SessionManager>>,
    #[cfg(feature = "mcpd")]
    mcp: Option<mcp::McpEndpoint>,
}

impl ServerState {
    async fn shutdown_sessions(&self) {
        #[cfg(feature = "mcpd")]
        if let Some(mcp) = &self.mcp {
            mcp.shutdown().await;
        }
        if let Some(sessions) = &self.sessions {
            sessions.close_all();
            // Supervisors need the complete process tree to attest descendant
            // containment and commit handoff. Global cleanup removes registry
            // entries, so it must not race that proof.
            sessions.wait_stopped().await;
            self.process_table
                .shutdown_all_processes_bounded(Duration::from_secs(30))
                .await;
        }
    }
}

/// Entry point called from `bin/chaos` when `chaos serve` is dispatched.
pub async fn run_main(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    serve_cli: ServeCli,
) -> anyhow::Result<()> {
    #[cfg(feature = "mcpd")]
    mcp::validate_cli(&serve_cli)?;
    // 1. Validate bearer token and numeric settings.
    let bearer_token: Arc<str> = match serve_cli.bearer_token.as_deref() {
        Some(t) if !t.is_empty() => Arc::from(t),
        _ => bail!("bearer token is required: set --bearer-token or CHAOS_BEARER_TOKEN"),
    };
    let recovery_token: Option<Arc<str>> = match serve_cli.recovery_token.as_deref() {
        Some(token) => {
            if token.trim().is_empty() || token == bearer_token.as_ref() {
                bail!("recovery token must be nonempty and different from the bearer token");
            }
            if serve_cli.session_scope.is_none() {
                bail!("--recovery-token requires --session-scope");
            }
            Some(Arc::from(token))
        }
        None => None,
    };
    if serve_cli.session_recovery_only
        && (serve_cli.session_scope.is_none() || recovery_token.is_none())
    {
        bail!("--session-recovery-only requires --session-scope and --recovery-token");
    }
    let webhook = match serve_cli.session_webhook_url.as_deref() {
        Some(url) => {
            if serve_cli.session_scope.is_none() {
                bail!("--session-webhook-url requires --session-scope");
            }
            Some(webhooks::WebhookConfig::from_env(
                url,
                &serve_cli.session_webhook_secret_env,
                &bearer_token,
                recovery_token.as_deref(),
            )?)
        }
        None => None,
    };
    if serve_cli.max_concurrent == 0 {
        bail!("--max-concurrent must be at least 1");
    }
    if serve_cli.timeout == 0 {
        bail!("--timeout must be at least 1");
    }
    if serve_cli.body_limit == 0 {
        bail!("--body-limit must be at least 1");
    }
    if serve_cli.max_sessions == 0 || serve_cli.session_ttl == 0 {
        bail!("--max-sessions and --session-ttl must be at least 1");
    }
    if let Some(scope) = &serve_cli.session_scope {
        if !serve_cli.sessions || serve_cli.ephemeral {
            bail!("--session-scope requires --sessions and forbids --ephemeral");
        }
        if scope.trim().is_empty() || scope.len() > 256 {
            bail!("--session-scope must contain 1..256 bytes");
        }
    }

    // 2. Parse CLI -c overrides.
    let cli_kv_overrides = cli_config_overrides
        .parse_overrides()
        .map_err(|e| anyhow::anyhow!("error parsing -c overrides: {e}"))?;

    // 3. Resolve cwd.
    let resolved_cwd = serve_cli.cd.clone();

    // 4. Build sandbox mode.
    let sandbox_mode = serve_cli.sandbox.as_ref().map(|s| {
        let s: SandboxMode = (*s).into();
        s
    });

    // 5. Build ConfigOverrides.
    let overrides = ConfigOverrides {
        tui_output: false,
        model: serve_cli.model.clone(),
        review_model: None,
        config_profile: None,
        approval_policy: Some(ApprovalPolicy::Headless),
        approvals_reviewer: None,
        sandbox_mode,
        cwd: resolved_cwd,
        model_provider: None,
        provider_user_override: false,
        service_tier: None,
        alcatraz_exe: Some(arg0_paths.alcatraz_exe.clone()),
        base_instructions: None,
        developer_instructions: None,
        personality: None,
        compact_prompt: None,
        ephemeral: serve_cli.ephemeral.then_some(true),
        mcp_servers: None,
        active_project_trust: None,
        database_agent_roles: None,
        additional_writable_roots: Vec::new(),
    };

    // 6. Build the kernel config.
    let mut config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .build()
        .await?;
    if serve_cli.sessions && config.clamp && !serve_cli.session_recovery_only {
        bail!("retained HTTP sessions do not yet support clamp transports");
    }
    config.clamp_env_remove = vec![
        "CHAOS_BEARER_TOKEN".into(),
        "CHAOS_RECOVERY_TOKEN".into(),
        serve_cli.session_webhook_secret_env.clone(),
    ];
    if serve_cli.session_scope.is_some() && config.ephemeral {
        bail!("durable retained sessions cannot use ephemeral history");
    }

    // 7. Mount the storage backend for the life of the process.
    chaos_kern::runtime_db::mount_vfs_for_startup(&config).await?;

    if serve_cli.sessions && !serve_cli.session_recovery_only {
        webhooks::WebhookConfig::check_control_plane_environment(
            &config,
            &[
                "CHAOS_BEARER_TOKEN",
                "CHAOS_RECOVERY_TOKEN",
                serve_cli.session_webhook_secret_env.as_str(),
            ],
        )?;
    }

    // 8. Enforce login restrictions.
    if !serve_cli.session_recovery_only {
        chaos_kern::auth::enforce_login_restrictions(&config)?;
    }

    // 8. Git/trust check.
    if !serve_cli.session_recovery_only
        && !serve_cli.skip_git_repo_check
        && chaos_kern::git_info::get_git_repo_root(&config.cwd).is_none()
    {
        bail!("not inside a git repository (use --skip-git-repo-check to override)");
    }

    // 9. Initialize tracing/otel.
    let otel = chaos_kern::otel_init::build_provider(
        &config,
        CHAOS_VERSION,
        Some(OTEL_SERVICE_NAME),
        DEFAULT_ANALYTICS_ENABLED,
    )
    .ok()
    .flatten();

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::from_default_env());
    let otel_logger_layer = otel.as_ref().and_then(|p| p.logger_layer());
    let otel_tracing_layer = otel.as_ref().and_then(|p| p.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    // 10. Boot CoreBoot.
    let core = CoreBoot::boot(
        &config,
        SessionSource::Api,
        CollaborationModesConfig {
            default_mode_request_user_input: false,
        },
    );

    let config = Arc::new(config);
    #[cfg(feature = "mcpd")]
    let mcp = if serve_cli.mcp {
        let handle = chaos_mcpd::build_server(config.clone(), arg0_paths).await?;
        Some(mcp::McpEndpoint::new(handle, &serve_cli, &bearer_token)?)
    } else {
        None
    };
    let policy = if serve_cli.session_scope.is_some() && !serve_cli.session_recovery_only {
        Some(Arc::new(
            sessions::durable::policy::PinnedPolicy::new(config.clone(), core.auth_manager.clone())
                .map_err(|error| anyhow::anyhow!("pin HTTP session policy: {error:?}"))?,
        ))
    } else {
        None
    };
    let semaphore = Arc::new(Semaphore::new(serve_cli.max_concurrent));
    let (sessions, recovered) = if let Some(scope) = serve_cli.session_scope {
        let client = chaos_kern::journal_client_for_mounted_backend()
            .await
            .map_err(anyhow::Error::msg)?;
        let (manager, recovered) = sessions::SessionManager::persistent(
            serve_cli.max_sessions,
            Duration::from_secs(serve_cli.session_ttl),
            client,
            scope,
            &semaphore,
            policy,
            serve_cli.session_recovery_only,
        )
        .await
        .map_err(|error| anyhow::anyhow!("restore HTTP sessions: {error:?}"))?;
        (Some(Arc::new(manager)), recovered)
    } else {
        (
            serve_cli.sessions.then(|| {
                Arc::new(sessions::SessionManager::new(
                    serve_cli.max_sessions,
                    Duration::from_secs(serve_cli.session_ttl),
                ))
            }),
            Vec::new(),
        )
    };

    if let Some(webhook) = &webhook {
        webhook
            .configure(sessions.as_ref().expect("durable scope validated"))
            .await?;
    }

    // 11. Build server state.
    let state = Arc::new(ServerState {
        config,
        process_table: core.process_table,
        auth_manager: core.auth_manager,
        bearer_token,
        recovery_token,
        semaphore,
        max_concurrent: serve_cli.max_concurrent,
        timeout: Duration::from_secs(serve_cli.timeout),
        body_limit: serve_cli.body_limit,
        monitor: monitor::MonitorState::new(),
        sessions,
        #[cfg(feature = "mcpd")]
        mcp,
    });
    for (session, commands, request) in recovered {
        let lazy = request.resume_process_id.is_some();
        sessions::runtime::spawn(state.clone(), session, commands, request, lazy);
    }

    // 12. Bind and serve.
    let ip: std::net::IpAddr = serve_cli.bind.parse().context("invalid bind address")?;
    let addr = SocketAddr::new(ip, serve_cli.port);
    state.monitor.publish(
        monitor::MonitorEventKind::ServerStarted,
        None,
        None,
        Some(addr.to_string()),
    );

    let shutdown_state = state.clone();
    let graceful = graceful::Shutdown::new(async move {
        let mut signal = Box::pin(graceful::default_signal());
        signal.as_mut().await;
        // Close retained streams/processes before waiting for HTTP drainage.
        // Otherwise an SSE subscriber could keep graceful shutdown alive.
        shutdown_state.shutdown_sessions().await;
    });
    let exec = Executor::graceful(graceful.guard());

    let listener = TcpListener::build(exec.clone())
        .bind_address(addr)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))
        .with_context(|| format!("bind chaos-httpd: {addr}"))?;

    let _webhook_task = webhook.map(|webhook| {
        webhook.spawn(
            state
                .sessions
                .as_ref()
                .expect("durable scope validated")
                .clone(),
        )
    });
    let service = api::http_service(state.clone());

    info!(
        bind = %addr,
        version = CHAOS_VERSION,
        "chaos-httpd listening",
    );

    let http_service = HttpServer::new_http1(exec).service(service);

    let mut serve_task = tokio::spawn(async move {
        listener.serve(http_service).await;
        Ok::<(), anyhow::Error>(())
    });

    tokio::select! {
        result = &mut serve_task => {
            state.shutdown_sessions().await;
            return match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(anyhow::anyhow!("chaos-httpd serve error: {err}")),
                Err(err) => Err(err).context("join chaos-httpd server task"),
            };
        }
        shutdown_delay = graceful.shutdown() => {
            info!(
                shutdown_delay_ms = shutdown_delay.as_millis(),
                "chaos-httpd shutting down",
            );
        }
    }

    state.shutdown_sessions().await;

    match serve_task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(anyhow::anyhow!("chaos-httpd serve error: {err}")),
        Err(err) => Err(err).context("join chaos-httpd server task"),
    }
}
