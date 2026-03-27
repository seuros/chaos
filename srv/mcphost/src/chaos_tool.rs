//! Unified `chaos` MCP tool — replaces the old `codex` + `codex-reply` pair.
//!
//! Omit `process_id` → new process via ProcessTable.
//! Provide `process_id` → resume an existing process.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use chaos_argv::Arg0DispatchPaths;
use chaos_kern::ProcessTable;
use chaos_kern::config::Config;
use chaos_kern::config::ConfigOverrides;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::SandboxMode;
use chaos_ipc::protocol::AskForApproval;
use chaos_conv::json_to_toml;
use mcp_host::prelude::*;
use mcp_host::registry::router::{McpToolRouter, tool_info_with_output};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;

use crate::chaos_runner;
use crate::outgoing_message::OutgoingMessageSender;

/// Server state shared across all MCP tool invocations.
pub(crate) struct ChaosMcpServer {
    pub(crate) process_table: Arc<ProcessTable>,
    pub(crate) outgoing: Arc<OutgoingMessageSender>,
    pub(crate) arg0_paths: Arg0DispatchPaths,
    /// Maps active MCP request IDs → active process IDs for cancellation.
    pub(crate) running_requests: Arc<Mutex<HashMap<RequestId, ProcessId>>>,
    /// Maps MCP session IDs → last used process ID for auto-resume.
    pub(crate) session_processes: Arc<Mutex<HashMap<String, ProcessId>>>,
    /// Caches process names from ProcessNameUpdated events.
    pub(crate) process_names: Arc<Mutex<HashMap<ProcessId, String>>>,
    /// State database for persisted process metadata.
    pub(crate) state_runtime: Option<chaos_kern::state_db::StateDbHandle>,
}

// ---------------------------------------------------------------------------
// Tool parameters (schemars 0.8 — workspace version)
// ---------------------------------------------------------------------------

/// Parameters for the `chaos` tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "kebab-case")]
pub struct ChaosToolParams {
    /// The user prompt to send to Codex.
    pub prompt: String,

    /// Process ID to resume a specific process.
    /// Omit to auto-resume the last process for this session, or start a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_id: Option<String>,

    /// Optional model override (e.g. 'gpt-5.4', 'claude-sonnet-4-6').
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Configuration profile from config.toml.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,

    /// Working directory for the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,

    /// Approval policy: `untrusted`, `on-failure`, `on-request`, `never`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<ChaosApprovalPolicy>,

    /// Sandbox mode: `read-only`, `workspace-write`, `root-access`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<ChaosSandboxMode>,

    /// Individual config overrides (keys from config.toml).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<HashMap<String, serde_json::Value>>,

    /// Replace default system instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_instructions: Option<String>,

    /// Developer instructions injected as a developer role message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developer_instructions: Option<String>,

    /// Prompt used when compacting the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ChaosApprovalPolicy {
    Untrusted,
    OnFailure,
    OnRequest,
    Never,
}

impl From<ChaosApprovalPolicy> for AskForApproval {
    fn from(value: ChaosApprovalPolicy) -> Self {
        match value {
            ChaosApprovalPolicy::Untrusted => AskForApproval::UnlessTrusted,
            ChaosApprovalPolicy::OnFailure => AskForApproval::OnFailure,
            ChaosApprovalPolicy::OnRequest => AskForApproval::OnRequest,
            ChaosApprovalPolicy::Never => AskForApproval::Never,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ChaosSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    RootAccess,
}

impl From<ChaosSandboxMode> for SandboxMode {
    fn from(value: ChaosSandboxMode) -> Self {
        match value {
            ChaosSandboxMode::ReadOnly => SandboxMode::ReadOnly,
            ChaosSandboxMode::WorkspaceWrite => SandboxMode::WorkspaceWrite,
            ChaosSandboxMode::RootAccess => SandboxMode::RootAccess,
        }
    }
}

impl ChaosToolParams {
    /// Build a `Config` from the supplied parameters and return (prompt, config).
    pub async fn into_config(
        self,
        arg0_paths: Arg0DispatchPaths,
    ) -> std::io::Result<(String, Config)> {
        let Self {
            prompt,
            process_id: _,
            model,
            profile,
            cwd,
            approval_policy,
            sandbox,
            config: cli_overrides,
            base_instructions,
            developer_instructions,
            compact_prompt,
        } = self;

        let overrides = ConfigOverrides {
            model,
            config_profile: profile,
            cwd: cwd.map(PathBuf::from),
            approval_policy: approval_policy.map(Into::into),
            sandbox_mode: sandbox.map(Into::into),
            alcatraz_linux_exe: arg0_paths.alcatraz_linux_exe.clone(),
            alcatraz_freebsd_exe: arg0_paths.alcatraz_freebsd_exe.clone(),
            main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
            base_instructions,
            developer_instructions,
            compact_prompt,
            ..Default::default()
        };

        let cli_overrides = cli_overrides
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, json_to_toml(v)))
            .collect();

        let cfg =
            Config::load_with_cli_overrides_and_harness_overrides(cli_overrides, overrides).await?;

        Ok((prompt, cfg))
    }
}

// ---------------------------------------------------------------------------
// Tool handler — manual registration (avoids schemars version conflict)
// ---------------------------------------------------------------------------

impl ChaosMcpServer {
    async fn handle_chaos<'a>(&self, ctx: ExecutionContext<'a>) -> Result<ToolOutput, ToolError> {
        let args = ctx.params.clone();
        let params: ChaosToolParams = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("invalid params: {e}")))?;

        let session_id = ctx.session.id.clone();
        let request_id = RequestId::String(session_id.clone());

        // Resolve process_id: explicit > auto-resume from session > new
        // Pass "new" or "" to force a new process instead of auto-resuming.
        let existing_process_id = match &params.process_id {
            Some(pid) if pid.is_empty() || pid.eq_ignore_ascii_case("new") => None,
            Some(pid) => Some(
                ProcessId::from_string(pid)
                    .map_err(|e| ToolError::Execution(format!("invalid process_id: {e}")))?,
            ),
            None => {
                // Auto-resume: reuse the last process for this session
                self.session_processes.lock().await.get(&session_id).copied()
            }
        };

        // Build config only for new processes
        let (prompt, config) = if existing_process_id.is_some() {
            (params.prompt, None)
        } else {
            let (prompt, cfg) = params
                .into_config(self.arg0_paths.clone())
                .await
                .map_err(|e| ToolError::Execution(format!("failed to load config: {e}")))?;
            (prompt, Some(cfg))
        };

        let progress_token = ctx.progress_token().map(String::from);

        let outcome = chaos_runner::run_chaos_session(
            request_id,
            prompt,
            config,
            existing_process_id,
            self.outgoing.clone(),
            self.process_table.clone(),
            self.running_requests.clone(),
            self.process_names.clone(),
            progress_token,
        )
        .await;

        // Track the process for auto-resume on next call from this session.
        self.session_processes
            .lock()
            .await
            .insert(session_id, outcome.process_id);

        if outcome.is_error {
            Err(ToolError::Execution(outcome.text))
        } else {
            structured(json!({
                "processId": outcome.process_id.to_string(),
                "content": outcome.text,
            }))
        }
    }
}

// ---------------------------------------------------------------------------
// Schema and router
// ---------------------------------------------------------------------------

fn chaos_input_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(ChaosToolParams);
    let mut schema_value = serde_json::to_value(&schema).expect("schema serializes");

    // Extract only the keys MCP needs.
    if let serde_json::Value::Object(ref mut obj) = schema_value {
        let mut input_schema = serde_json::Map::new();
        for key in ["properties", "required", "type", "$defs", "definitions"] {
            if let Some(value) = obj.remove(key) {
                input_schema.insert(key.to_string(), value);
            }
        }
        return serde_json::Value::Object(input_schema);
    }
    schema_value
}

fn chaos_output_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "processId": { "type": "string" },
            "content": { "type": "string" }
        },
        "required": ["processId", "content"],
    })
}

fn chaos_tool_info() -> ToolInfo {
    tool_info_with_output(
        "chaos",
        None,
        Some("Run a Chaos session. Auto-resumes the last process per session; pass process-id to target a specific one.".to_string()),
        chaos_input_schema(),
        chaos_output_schema(),
    )
}

fn chaos_handler<'a>(
    server: &'a ChaosMcpServer,
    ctx: ExecutionContext<'a>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>
{
    Box::pin(server.handle_chaos(ctx))
}

pub(crate) fn tool_router() -> McpToolRouter<ChaosMcpServer> {
    McpToolRouter::new().with_tool(chaos_tool_info(), chaos_handler, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn verify_chaos_tool_json_schema() {
        let tool = chaos_tool_info();
        let tool_json = serde_json::to_value(&tool).expect("tool serializes");
        // Verify required fields exist.
        let input = tool_json.get("inputSchema").expect("inputSchema");
        let props = input.get("properties").expect("properties");
        assert!(props.get("prompt").is_some(), "prompt field required");
        assert!(props.get("process-id").is_some(), "process-id field required");
        assert_eq!(tool_json.get("name"), Some(&json!("chaos")));
    }
}
