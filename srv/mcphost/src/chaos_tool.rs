//! Unified `chaos` MCP tool — replaces the old `codex` + `codex-reply` pair.
//!
//! Omit `thread_id` → new thread via ThreadManager.
//! Provide `thread_id` → resume existing thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use codex_arg0::Arg0DispatchPaths;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_core::config::ConfigOverrides;
use codex_protocol::ThreadId;
use codex_protocol::config_types::SandboxMode;
use codex_protocol::protocol::AskForApproval;
use codex_utils_json_to_toml::json_to_toml;
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
    pub(crate) thread_manager: Arc<ThreadManager>,
    pub(crate) outgoing: Arc<OutgoingMessageSender>,
    pub(crate) arg0_paths: Arg0DispatchPaths,
    /// Maps active MCP request IDs → Codex thread IDs for cancellation.
    pub(crate) running_requests: Arc<Mutex<HashMap<RequestId, ThreadId>>>,
    /// Maps MCP session IDs → last used thread ID for auto-resume.
    pub(crate) session_threads: Arc<Mutex<HashMap<String, ThreadId>>>,
    /// Caches thread names from ThreadNameUpdated events.
    pub(crate) thread_names: Arc<Mutex<HashMap<ThreadId, String>>>,
    /// State database for persisted thread metadata.
    pub(crate) state_runtime: Option<codex_core::state_db::StateDbHandle>,
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

    /// Thread ID to resume a specific conversation.
    /// Omit to auto-resume the last thread for this session, or start a new one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,

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

    /// Sandbox mode: `read-only`, `workspace-write`, `danger-full-access`.
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
    DangerFullAccess,
}

impl From<ChaosSandboxMode> for SandboxMode {
    fn from(value: ChaosSandboxMode) -> Self {
        match value {
            ChaosSandboxMode::ReadOnly => SandboxMode::ReadOnly,
            ChaosSandboxMode::WorkspaceWrite => SandboxMode::WorkspaceWrite,
            ChaosSandboxMode::DangerFullAccess => SandboxMode::DangerFullAccess,
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
            thread_id: _,
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

        // Resolve thread_id: explicit > auto-resume from session > new
        // Pass "new" or "" to force a new thread instead of auto-resuming.
        let existing_thread_id = match &params.thread_id {
            Some(tid) if tid.is_empty() || tid.eq_ignore_ascii_case("new") => None,
            Some(tid) => Some(
                ThreadId::from_string(tid)
                    .map_err(|e| ToolError::Execution(format!("invalid thread_id: {e}")))?,
            ),
            None => {
                // Auto-resume: reuse last thread for this session
                self.session_threads.lock().await.get(&session_id).copied()
            }
        };

        // Build config only for new threads
        let (prompt, config) = if existing_thread_id.is_some() {
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
            existing_thread_id,
            self.outgoing.clone(),
            self.thread_manager.clone(),
            self.running_requests.clone(),
            self.thread_names.clone(),
            progress_token,
        )
        .await;

        // Track thread for auto-resume on next call from this session.
        self.session_threads
            .lock()
            .await
            .insert(session_id, outcome.thread_id);

        if outcome.is_error {
            Err(ToolError::Execution(outcome.text))
        } else {
            structured(json!({
                "threadId": outcome.thread_id.to_string(),
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
            "threadId": { "type": "string" },
            "content": { "type": "string" }
        },
        "required": ["threadId", "content"],
    })
}

fn chaos_tool_info() -> ToolInfo {
    tool_info_with_output(
        "chaos",
        None,
        Some("Run a Chaos session. Auto-resumes last thread per session; pass thread-id to target a specific one.".to_string()),
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
        assert!(props.get("thread-id").is_some(), "thread-id field required");
        assert_eq!(tool_json.get("name"), Some(&json!("chaos")));
    }
}
