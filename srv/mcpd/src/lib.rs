//! Chaos MCP server — built on mcp-host.
#![deny(clippy::print_stdout, clippy::print_stderr)]

use std::collections::HashMap;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_argv::Arg0DispatchPaths;
use chaos_getopt::CliConfigOverrides;
use chaos_ipc::product::CHAOS_VERSION;
use chaos_ipc::protocol::SessionSource;
use chaos_kern::AuthManager;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use chaos_kern::config::types::McpServerConfig;
use chaos_kern::config::types::McpServerTransportConfig;
use chaos_kern::models_manager::collaboration_mode_presets::CollaborationModesConfig;
use mcp_host::prelude::*;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

/// Environment variable used to detect when `chaos mcp serve` is running
/// inside another `chaos mcp serve` — see [`guard_against_recursive_mcpd`].
const CHAOS_MCPD_DEPTH_ENV: &str = "CHAOS_MCPD_DEPTH";

/// Hard cap on nested chaos-mcpd instances. One level of nesting is enough
/// to support running `chaos mcp serve` from inside a chaos session that
/// spawned it (e.g. a debugging scenario); anything beyond that is
/// guaranteed to be a configuration-induced skynet attack (or AI overtake).
const CHAOS_MCPD_MAX_DEPTH: u32 = 1;

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
const OTEL_SERVICE_NAME: &str = "chaos_mcpd";

pub async fn run_main(
    arg0_paths: Arg0DispatchPaths,
    cli_config_overrides: CliConfigOverrides,
) -> IoResult<()> {
    // Refuse to boot if we're already running inside another chaos-mcpd
    // beyond the permitted nesting depth. Catches the fork-bomb / skynet
    // scenario where a project `.mcp.json` lists `chaos mcp serve` as one
    // of its servers and each nested session spawns another sidecar.
    guard_against_recursive_mcpd()?;

    // Parse CLI overrides and load base Config.
    let cli_kv_overrides = cli_config_overrides.parse_overrides().map_err(|e| {
        std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("error parsing -c overrides: {e}"),
        )
    })?;
    let mut config = Config::load_with_cli_overrides(cli_kv_overrides)
        .await
        .map_err(|e| {
            std::io::Error::new(ErrorKind::InvalidData, format!("error loading config: {e}"))
        })?;

    // Second layer of defense: strip any `chaos mcp serve` self-reference
    // from the merged mcp_servers table. Project `.mcp.json` files in the
    // wild list `chaos` as a server (useful when editing from Claude Code
    // or another host), and we don't want to spawn a child chaos-mcpd from
    // our own session.
    strip_chaos_self_reference_from_mcp_servers(&mut config);

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

    // Init runtime database singleton — same DB as TUI/CLI.
    let config = Arc::new(config);
    let runtime_db = chaos_kern::runtime_db::get_runtime_db(&config).await;

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
            default_mode_request_user_input: true,
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
        runtime_db,
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

/// Returns `Err(ErrorKind::WouldBlock)` if the current process is already
/// nested deeper than `CHAOS_MCPD_MAX_DEPTH` layers of `chaos mcp serve`.
/// On success, bumps the depth counter so children inherit the new value.
fn guard_against_recursive_mcpd() -> IoResult<()> {
    let depth: u32 = std::env::var(CHAOS_MCPD_DEPTH_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    if depth > CHAOS_MCPD_MAX_DEPTH {
        return Err(std::io::Error::new(
            ErrorKind::WouldBlock,
            format!(
                "chaos mcp serve refusing to start: already nested {depth} level(s) deep \
                 (max {CHAOS_MCPD_MAX_DEPTH}). This means a parent chaos session spawned \
                 us as an MCP server — check your project `.mcp.json` or user config for \
                 a `chaos` server entry that points at this binary."
            ),
        ));
    }

    // SAFETY: single-threaded at startup; nothing else is reading env yet.
    // `set_var` is `unsafe` on newer rustc because concurrent getenv is UB
    // on some platforms, but we're pre-tokio-runtime here.
    unsafe {
        std::env::set_var(CHAOS_MCPD_DEPTH_ENV, (depth + 1).to_string());
    }
    Ok(())
}

/// Canonicalizes a path without panicking if it doesn't exist. Falls back
/// to the original path when canonicalization fails so caller-side equality
/// checks still work against non-existent paths (e.g. removed binaries).
fn canonicalize_lossy(path: &std::path::Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn resolve_command_path(command_path: &std::path::Path) -> Option<PathBuf> {
    if command_path.components().count() > 1 {
        return Some(canonicalize_lossy(command_path));
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(command_path))
        .find(|candidate| candidate.exists())
        .map(|candidate| canonicalize_lossy(&candidate))
}

/// Filter `config.mcp_servers` in place, removing any stdio entry that
/// resolves to the current chaos binary invoking `mcp serve`. Anything
/// else (dictator, necromancer, third-party servers, streamable-http) is
/// left untouched.
fn strip_chaos_self_reference_from_mcp_servers(config: &mut Config) {
    let self_exe = std::env::current_exe().ok().map(|p| canonicalize_lossy(&p));
    let self_stem = self_exe
        .as_ref()
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .map(str::to_string);

    let original: HashMap<String, McpServerConfig> = config.mcp_servers.get().clone();
    let mut filtered = HashMap::with_capacity(original.len());
    let mut dropped: Vec<String> = Vec::new();

    for (name, entry) in original {
        if is_chaos_self_reference(&entry, self_exe.as_deref(), self_stem.as_deref()) {
            dropped.push(name);
        } else {
            filtered.insert(name, entry);
        }
    }

    for name in &dropped {
        tracing::warn!(
            server = %name,
            "dropping chaos self-reference from mcp_servers to avoid recursive chaos-mcpd spawn"
        );
    }

    if !dropped.is_empty()
        && let Err(e) = config.mcp_servers.set(filtered)
    {
        tracing::error!(
            error = %e,
            "failed to apply filtered mcp_servers after stripping self-references"
        );
    }
}

fn is_chaos_self_reference(
    entry: &McpServerConfig,
    self_exe: Option<&std::path::Path>,
    self_stem: Option<&str>,
) -> bool {
    let McpServerTransportConfig::Stdio { command, args, .. } = &entry.transport else {
        return false;
    };

    // Only `mcp serve` (or `mcp` alone, which the cli also routes to serve
    // in future subcommands) counts as recursive. Plain `chaos exec ...`
    // etc. is fine — it's a different role.
    let invokes_mcp = args.iter().any(|a| a == "mcp");
    if !invokes_mcp {
        return false;
    }

    let command_path = std::path::Path::new(command);
    let resolved_command = resolve_command_path(command_path);

    if let Some(exe) = self_exe
        && resolved_command.as_deref() == Some(exe)
    {
        return true;
    }

    // Fallback: match unresolved bare `chaos` by basename. Explicit paths
    // that resolve elsewhere on PATH are not self-references.
    if let Some(stem) = self_stem
        && resolved_command.is_none()
        && command_path.components().count() == 1
        && let Some(cmd_stem) = command_path.file_stem().and_then(|s| s.to_str())
        && cmd_stem == stem
    {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_kern::config::ConfigBuilder;
    use chaos_kern::config::types::OtelExporterKind;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::sync::{LazyLock, Mutex};
    use tempfile::TempDir;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn mcp_server_defaults_analytics_to_enabled() {
        assert_eq!(DEFAULT_ANALYTICS_ENABLED, true);
    }

    fn stdio_server(command: &str, args: &[&str]) -> McpServerConfig {
        McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: command.to_string(),
                args: args.iter().map(|s| (*s).to_string()).collect(),
                env: None,
                env_vars: Vec::new(),
                cwd: None,
            },
            enabled: true,
            required: false,
            disabled_reason: None,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
            scopes: None,
            oauth_resource: None,
            r#type: None,
            oauth: None,
        }
    }

    #[test]
    fn is_chaos_self_reference_matches_exact_path_with_mcp_arg() {
        let exe = PathBuf::from("/usr/local/bin/chaos");
        let entry = stdio_server("/usr/local/bin/chaos", &["mcp", "serve"]);
        assert!(is_chaos_self_reference(
            &entry,
            Some(exe.as_path()),
            Some("chaos"),
        ));
    }

    #[test]
    fn is_chaos_self_reference_matches_basename_when_path_unresolved() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let saved_path = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", OsString::new());
        }
        let exe = PathBuf::from("/does/not/exist/chaos");
        // Bare `chaos` with no PATH resolution available — basename fallback hits.
        let entry = stdio_server("chaos", &["mcp", "serve"]);
        let is_self = is_chaos_self_reference(&entry, Some(exe.as_path()), Some("chaos"));
        unsafe {
            match saved_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(is_self);
    }

    #[test]
    fn is_chaos_self_reference_matches_resolved_path_on_path() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let saved_path = std::env::var_os("PATH");
        let temp = TempDir::new().unwrap();
        let self_dir = temp.path().join("self");
        std::fs::create_dir_all(&self_dir).unwrap();
        let self_exe = self_dir.join("chaos");
        std::fs::write(&self_exe, b"#!/bin/sh\n").unwrap();
        unsafe {
            std::env::set_var("PATH", &self_dir);
        }

        let entry = stdio_server("chaos", &["mcp", "serve"]);
        let is_self = is_chaos_self_reference(&entry, Some(self_exe.as_path()), Some("chaos"));

        unsafe {
            match saved_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(is_self);
    }

    #[test]
    fn is_chaos_self_reference_ignores_different_chaos_on_path() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let saved_path = std::env::var_os("PATH");
        let temp = TempDir::new().unwrap();
        let self_dir = temp.path().join("self");
        let other_dir = temp.path().join("other");
        std::fs::create_dir_all(&self_dir).unwrap();
        std::fs::create_dir_all(&other_dir).unwrap();

        let self_exe = self_dir.join("chaos");
        let other_exe = other_dir.join("chaos");
        std::fs::write(&self_exe, b"#!/bin/sh\n").unwrap();
        std::fs::write(&other_exe, b"#!/bin/sh\n").unwrap();
        unsafe {
            std::env::set_var("PATH", &other_dir);
        }

        let entry = stdio_server("chaos", &["mcp", "serve"]);
        let is_self = is_chaos_self_reference(&entry, Some(self_exe.as_path()), Some("chaos"));

        unsafe {
            match saved_path {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
        assert!(!is_self);
    }

    #[test]
    fn is_chaos_self_reference_ignores_chaos_without_mcp_arg() {
        let exe = PathBuf::from("/usr/local/bin/chaos");
        // `chaos exec foo` is a different role — do not strip.
        let entry = stdio_server("/usr/local/bin/chaos", &["exec", "foo"]);
        assert!(!is_chaos_self_reference(
            &entry,
            Some(exe.as_path()),
            Some("chaos"),
        ));
    }

    #[test]
    fn is_chaos_self_reference_ignores_unrelated_binaries() {
        let exe = PathBuf::from("/usr/local/bin/chaos");
        let entry = stdio_server("dictator", &["mcp"]);
        assert!(!is_chaos_self_reference(
            &entry,
            Some(exe.as_path()),
            Some("chaos"),
        ));
    }

    #[test]
    fn is_chaos_self_reference_ignores_streamable_http() {
        let exe = PathBuf::from("/usr/local/bin/chaos");
        let entry = McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "http://localhost:3011/mcp".to_string(),
                bearer_token_env_var: None,
                http_headers: None,
                env_http_headers: None,
            },
            ..stdio_server("unused", &[])
        };
        assert!(!is_chaos_self_reference(
            &entry,
            Some(exe.as_path()),
            Some("chaos"),
        ));
    }

    #[test]
    fn guard_against_recursive_mcpd_allows_first_nested_level() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let saved = std::env::var(CHAOS_MCPD_DEPTH_ENV).ok();
        unsafe {
            std::env::set_var(CHAOS_MCPD_DEPTH_ENV, CHAOS_MCPD_MAX_DEPTH.to_string());
        }
        let result = guard_against_recursive_mcpd();
        assert!(result.is_ok(), "expected first nested server to be allowed");
        assert_eq!(
            std::env::var(CHAOS_MCPD_DEPTH_ENV),
            Ok((CHAOS_MCPD_MAX_DEPTH + 1).to_string())
        );
        unsafe {
            match saved {
                Some(v) => std::env::set_var(CHAOS_MCPD_DEPTH_ENV, v),
                None => std::env::remove_var(CHAOS_MCPD_DEPTH_ENV),
            }
        }
    }

    #[test]
    fn guard_against_recursive_mcpd_refuses_beyond_max_depth() {
        let _env_lock = ENV_LOCK.lock().unwrap();
        let saved = std::env::var(CHAOS_MCPD_DEPTH_ENV).ok();
        unsafe {
            std::env::set_var(CHAOS_MCPD_DEPTH_ENV, (CHAOS_MCPD_MAX_DEPTH + 1).to_string());
        }
        let result = guard_against_recursive_mcpd();
        assert!(result.is_err(), "expected recursion guard to trip");
        assert_eq!(result.err().unwrap().kind(), ErrorKind::WouldBlock);
        unsafe {
            match saved {
                Some(v) => std::env::set_var(CHAOS_MCPD_DEPTH_ENV, v),
                None => std::env::remove_var(CHAOS_MCPD_DEPTH_ENV),
            }
        }
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
