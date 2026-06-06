//! chaos-httpd — HTTP trigger server for Chaos.
#![deny(clippy::print_stdout, clippy::print_stderr)]

mod api;
mod auth;
pub mod cli;
mod monitor;
pub mod protocol;
mod runner;

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
use rama::graceful;
use rama::http::server::HttpServer;
use rama::rt::Executor;
use rama::tcp::server::TcpListener;
use tokio::sync::Semaphore;
use tracing::info;
use tracing_subscriber::EnvFilter;
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
    semaphore: Arc<Semaphore>,
    max_concurrent: usize,
    timeout: Duration,
    body_limit: usize,
    monitor: monitor::MonitorState,
}

/// Entry point called from `bin/chaos` when `chaos serve` is dispatched.
pub async fn run_main(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
    serve_cli: ServeCli,
) -> anyhow::Result<()> {
    // 1. Validate bearer token and numeric settings.
    let bearer_token: Arc<str> = match serve_cli.bearer_token.as_deref() {
        Some(t) if !t.is_empty() => Arc::from(t),
        _ => bail!("bearer token is required: set --bearer-token or CHAOS_BEARER_TOKEN"),
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
        alcatraz_linux_exe: arg0_paths.alcatraz_linux_exe.clone(),
        alcatraz_freebsd_exe: arg0_paths.alcatraz_freebsd_exe.clone(),
        alcatraz_macos_exe: arg0_paths.alcatraz_macos_exe.clone(),
        base_instructions: None,
        minion_instructions: None,
        personality: None,
        compact_prompt: None,
        ephemeral: serve_cli.ephemeral.then_some(true),
        mcp_servers: None,
        active_project_trust: None,
        additional_writable_roots: Vec::new(),
    };

    // 6. Build the kernel config.
    let config = ConfigBuilder::default()
        .cli_overrides(cli_kv_overrides)
        .harness_overrides(overrides)
        .build()
        .await?;

    // 7. Enforce login restrictions.
    chaos_kern::auth::enforce_login_restrictions(&config)?;

    // 8. Git/trust check.
    if !serve_cli.skip_git_repo_check
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

    // 11. Build server state.
    let state = Arc::new(ServerState {
        config,
        process_table: core.process_table,
        auth_manager: core.auth_manager,
        bearer_token,
        semaphore: Arc::new(Semaphore::new(serve_cli.max_concurrent)),
        max_concurrent: serve_cli.max_concurrent,
        timeout: Duration::from_secs(serve_cli.timeout),
        body_limit: serve_cli.body_limit,
        monitor: monitor::MonitorState::new(),
    });

    // 12. Bind and serve.
    let ip: std::net::IpAddr = serve_cli.bind.parse().context("invalid bind address")?;
    let addr = SocketAddr::new(ip, serve_cli.port);
    state.monitor.publish(
        monitor::MonitorEventKind::ServerStarted,
        None,
        None,
        Some(addr.to_string()),
    );

    let graceful = graceful::Shutdown::new(async {
        let mut signal = Box::pin(graceful::default_signal());
        signal.as_mut().await;
    });
    let exec = Executor::graceful(graceful.guard());

    let listener = TcpListener::build(exec.clone())
        .bind_address(addr)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))
        .with_context(|| format!("bind chaos-httpd: {addr}"))?;

    let service = api::http_service(state);

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

    match serve_task.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(anyhow::anyhow!("chaos-httpd serve error: {err}")),
        Err(err) => Err(err).context("join chaos-httpd server task"),
    }
}
