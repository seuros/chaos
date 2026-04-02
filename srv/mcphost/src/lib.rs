//! Chaos MCP server — built on mcp-host.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::sync::Arc;

use chaos_argv::Arg0DispatchPaths;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::AuthManager;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use mcp_host::prelude::*;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

mod builtin_resources;
mod chaos_runner;
mod chaos_tool;
mod clamp_session_bridge;
mod elicitation;
mod exec_approval;
mod outgoing_message;
mod patch_approval;

use crate::chaos_tool::ChaosMcpServer;
use crate::outgoing_message::OutgoingMessage;
use crate::outgoing_message::OutgoingMessageSender;

pub use crate::chaos_tool::ChaosToolParams;
pub use crate::clamp_session_bridge::run_main as run_clamp_session_bridge_main;
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
    let otel = chaos_kern::otel_init::build_provider(
        &config,
        CHAOS_VERSION,
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
    let state_runtime = chaos_kern::state_db::get_state_db(&config).await;

    // Build ProcessTable.
    let auth_manager = AuthManager::shared(
        config.chaos_home.clone(),
        false,
        config.cli_auth_credentials_store_mode,
    );
    let process_table = Arc::new(ProcessTable::new(
        config.as_ref(),
        auth_manager,
        SessionSource::Mcp,
        CollaborationModesConfig {
            default_mode_request_user_input: config
                .features
                .enabled(chaos_kern::features::Feature::DefaultModeRequestUserInput),
        },
    ));

    // Outgoing message channel — used by the tool runner to send JSON-RPC
    // messages while mcp-host handles the transport framing.
    let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<OutgoingMessage>();
    let outgoing = Arc::new(OutgoingMessageSender::new(outgoing_tx));

    // Build the mcp-host Server.
    let mcp_server = server("chaos-mcp-server", CHAOS_VERSION)
        .with_tools(true)
        .with_resources(true, false)
        .with_resource_templates()
        .with_instructions("Chaos — provider-agnostic coding agent")
        .on_initialized({
            let outgoing = outgoing.clone();
            move |_session_id: String, requester: Option<ClientRequester>| {
                let outgoing = outgoing.clone();
                async move {
                    if let Some(req) = requester {
                        // Mirror elicitation support to our outgoing sender so approval
                        // handlers can check it without holding the requester lock.
                        if req.supports_elicitation() {
                            outgoing.set_client_elicitation_capability(Some(
                                &mcp_host::protocol::capabilities::ElicitationCapability::default(),
                            ));
                        }
                        // Store the requester so send_request can route server→client
                        // requests (e.g. elicitation/create) through mcp-host's transport.
                        outgoing.set_client_requester(req).await;
                    }
                }
            }
        })
        .build();

    // Register tools.
    let chaos_server = Arc::new(ChaosMcpServer {
        process_table,
        outgoing,
        arg0_paths,
        sqlite_home: config.sqlite_home.clone(),
        running_requests: Arc::new(Mutex::new(std::collections::HashMap::new())),
        session_processes: Arc::new(Mutex::new(std::collections::HashMap::new())),
        process_names: Arc::new(Mutex::new(std::collections::HashMap::new())),
        state_runtime,
    });
    chaos_tool::tool_router().register_all(mcp_server.tool_registry(), chaos_server.clone());
    builtin_resources::resource_router()
        .register_all(mcp_server.resource_manager(), chaos_server.clone());
    builtin_resources::resource_template_router()
        .register_all(mcp_server.resource_manager(), chaos_server);

    // Forward outgoing notifications and error responses to mcp-host's transport.
    // Server→client requests (e.g. `elicitation/create`) bypass this channel
    // entirely — they go through `OutgoingMessageSender::send_request` which
    // calls `ClientRequester::request_raw` directly.
    let notification_sender = mcp_server.notification_sender();
    tokio::spawn(async move {
        use crate::outgoing_message::OutgoingJsonRpcMessage;
        use mcp_host::protocol::types::JsonRpcMessage;

        while let Some(msg) = outgoing_rx.recv().await {
            let jsonrpc: OutgoingJsonRpcMessage = msg.into();
            match jsonrpc {
                JsonRpcMessage::Notification(n) => {
                    let notif = mcp_host::prelude::JsonRpcNotification::new(
                        n.method.clone(),
                        n.params.clone(),
                    );
                    let _ = notification_sender.send(notif);
                }
                JsonRpcMessage::Response(_) | JsonRpcMessage::Request(_) => {
                    // Unreachable: OutgoingMessage has no Request variant and
                    // errors are sent as Response. Guard kept for exhaustiveness.
                }
            }
        }
    });

    // Run with stdio transport.
    mcp_server
        .run(StdioTransport::new())
        .await
        .map_err(|e| std::io::Error::other(format!("mcp server error: {e}")))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_kern::config::ConfigBuilder;
    use chaos_kern::config::types::OtelExporterKind;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn mcp_server_defaults_analytics_to_enabled() {
        assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
    }

    #[tokio::test]
    async fn mcp_server_builds_otel_provider_with_logs_traces_and_metrics() -> anyhow::Result<()> {
        let chaos_home = TempDir::new()?;
        let mut config = ConfigBuilder::default()
            .chaos_home(chaos_home.path().to_path_buf())
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

        let provider = chaos_kern::otel_init::build_provider(
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
