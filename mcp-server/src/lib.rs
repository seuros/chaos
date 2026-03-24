//! Chaos MCP server — built on mcp-host.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::sync::Arc;

use codex_arg0::Arg0DispatchPaths;
use codex_core::AuthManager;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_core::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use codex_protocol::protocol::SessionSource;
use codex_utils_cli::CliConfigOverrides;
use mcp_host::prelude::*;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod chaos_tool;
mod chaos_runner;
mod elicitation;
mod exec_approval;
mod outgoing_message;
mod patch_approval;
mod session_resources;

use crate::chaos_tool::ChaosMcpServer;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;

pub use crate::chaos_tool::ChaosToolParams;
pub use crate::elicitation::ApprovalElicitationAction;
pub use crate::elicitation::ApprovalElicitationResponse;
pub use crate::exec_approval::ExecApprovalElicitRequestMeta;
pub use crate::exec_approval::ExecApprovalElicitRequestParams;
pub use crate::exec_approval::ExecApprovalResponse;
pub use crate::patch_approval::PatchApprovalElicitRequestMeta;
pub use crate::patch_approval::PatchApprovalElicitRequestParams;
pub use crate::patch_approval::PatchApprovalResponse;

const DEFAULT_ANALYTICS_ENABLED: bool = true;
const OTEL_SERVICE_NAME: &str = "chaos_mcphost";

pub async fn run_main(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
) -> IoResult<()> {
    // Parse CLI overrides and load base Config.
    let cli_kv_overrides = cli_config_overrides.parse_overrides().map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("error parsing -c overrides: {e}"),
        )
    })?;
    let config = Config::load_with_cli_overrides(cli_kv_overrides)
        .await
        .map_err(|e| {
            std::io::Error::new(ErrorKind::InvalidData, format!("error loading config: {e}"))
        })?;

    // OpenTelemetry setup.
    let otel = codex_core::otel_init::build_provider(
        &config,
        env!("CARGO_PKG_VERSION"),
        Some(OTEL_SERVICE_NAME),
        DEFAULT_ANALYTICS_ENABLED,
    )
    .map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidData,
            format!("error loading otel config: {e}"),
        )
    })?;

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_filter(EnvFilter::from_default_env());
    let otel_logger_layer = otel.as_ref().and_then(|provider| provider.logger_layer());
    let otel_tracing_layer = otel.as_ref().and_then(|provider| provider.tracing_layer());

    let _ = tracing_subscriber::registry()
        .with(fmt_layer)
        .with(otel_logger_layer)
        .with(otel_tracing_layer)
        .try_init();

    // Init state database singleton — same DB as TUI/CLI.
    let config = Arc::new(config);
    let state_runtime = codex_core::state_db::get_state_db(&config).await;

    // Build ThreadManager.
    let auth_manager = AuthManager::shared(
        config.codex_home.clone(),
        false,
        config.cli_auth_credentials_store_mode,
    );
    let thread_manager = Arc::new(ThreadManager::new(
        config.as_ref(),
        auth_manager,
        SessionSource::Mcp,
        CollaborationModesConfig {
            default_mode_request_user_input: config
                .features
                .enabled(codex_core::features::Feature::DefaultModeRequestUserInput),
        },
    ));

    // Outgoing message channel — used by the tool runner to send JSON-RPC
    // messages while mcp-host handles the transport framing.
    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing = Arc::new(OutgoingMessageSender::new(outgoing_tx));

    // Build the mcp-host Server.
    let mcp_server = server("chaos-mcp-server", env!("CARGO_PKG_VERSION"))
        .with_tools(true)
        .with_resources(true, false)
        .with_resource_templates()
        .with_instructions("Chaos — provider-agnostic coding agent")
        .on_initialized({
            let outgoing = outgoing.clone();
            move |_session_id: String, requester: Option<ClientRequester>| {
                // Capture elicitation support from the ClientRequester.
                // The ClientRequester knows what the client declared during initialize.
                if let Some(ref req) = requester {
                    // If the client supports form elicitation, mark it.
                    // ClientRequester tracks this internally; mirror it to our outgoing sender
                    // so approval handlers can check.
                    if req.supports_elicitation() {
                        outgoing.set_client_elicitation_capability(Some(
                            &mcp_host::protocol::capabilities::ElicitationCapability::default(),
                        ));
                    }
                }
                async {}
            }
        })
        .build();

    // Register tools.
    let chaos_server = Arc::new(ChaosMcpServer {
        thread_manager,
        outgoing,
        arg0_paths,
        running_requests: Arc::new(Mutex::new(std::collections::HashMap::new())),
        session_threads: Arc::new(Mutex::new(std::collections::HashMap::new())),
        thread_names: Arc::new(Mutex::new(std::collections::HashMap::new())),
        state_runtime,
    });
    chaos_tool::tool_router().register_all(mcp_server.tool_registry(), chaos_server.clone());
    session_resources::resource_router()
        .register_all(mcp_server.resource_manager(), chaos_server.clone());
    session_resources::resource_template_router()
        .register_all(mcp_server.resource_manager(), chaos_server);

    // Spawn a task to forward outgoing messages as notifications via mcp-host.
    let notification_sender = mcp_server.notification_sender();
    tokio::spawn(async move {
        use crate::outgoing_message::OutgoingJsonRpcMessage;
        use mcp_host::protocol::types::JsonRpcMessage;

        while let Some(msg) = outgoing_rx.recv().await {
            let jsonrpc: OutgoingJsonRpcMessage = msg.into();
            // Forward notifications and responses through mcp-host's notification channel.
            match &jsonrpc {
                JsonRpcMessage::Notification(n) => {
                    let notif = mcp_host::prelude::JsonRpcNotification::new(
                        n.method.clone(),
                        n.params.clone(),
                    );
                    let _ = notification_sender.send(notif);
                }
                JsonRpcMessage::Response(_) | JsonRpcMessage::Request(_) => {
                    // Error responses and server→client requests (elicitation)
                    // are not routed through mcp-host's notification channel.
                    tracing::debug!("non-notification message on outgoing channel (ignored)");
                }
            }
        }
    });

    // Run with stdio transport.
    mcp_server
        .run(StdioTransport::new())
        .await
        .map_err(|e| std::io::Error::new(ErrorKind::Other, format!("mcp server error: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::config::ConfigBuilder;
    use codex_core::config::types::OtelExporterKind;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn mcp_server_defaults_analytics_to_enabled() {
        assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
    }

    #[tokio::test]
    async fn mcp_server_builds_otel_provider_with_logs_traces_and_metrics() -> anyhow::Result<()> {
        let codex_home = TempDir::new()?;
        let mut config = ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .build()
            .await?;
        let exporter = OtelExporterKind::OtlpGrpc {
            endpoint: "http://localhost:4317".to_string(),
            headers: HashMap::new(),
            tls: None,
        };
        config.otel.exporter = exporter.clone();
        config.otel.trace_exporter = exporter.clone();
        config.otel.metrics_exporter = exporter;
        config.analytics_enabled = None;

        let provider = codex_core::otel_init::build_provider(
            &config,
            "0.0.0-test",
            Some(OTEL_SERVICE_NAME),
            DEFAULT_ANALYTICS_ENABLED,
        )
        .map_err(|err| anyhow::anyhow!(err.to_string()))?
        .expect("otel provider");

        assert!(provider.logger.is_some(), "expected log exporter");
        assert!(
            provider.tracer_provider.is_some(),
            "expected trace exporter"
        );
        assert!(provider.metrics().is_some(), "expected metrics exporter");
        provider.shutdown();

        Ok(())
    }
}
