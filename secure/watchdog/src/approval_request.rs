use std::path::PathBuf;

use chaos_ipc::approvals::NetworkApprovalProtocol;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::models::SandboxPermissions;
use chaos_realpath::AbsolutePathBuf;
use serde::Serialize;
use serde_json::Value;

/// Maximum number of approximate tokens to retain in a single action string
/// field when presenting a guardian action for review.
const GUARDIAN_MAX_ACTION_STRING_TOKENS: usize = 1_000;

/// Rough bytes-per-token ratio used for truncation budget estimates.
const APPROX_BYTES_PER_TOKEN: usize = 4;

/// Tag emitted in truncation markers.
const TRUNCATION_TAG: &str = "truncated";

#[derive(Debug, Clone, PartialEq)]
pub enum GuardianApprovalRequest {
    Shell {
        id: String,
        command: Vec<String>,
        cwd: PathBuf,
        sandbox_permissions: SandboxPermissions,
        additional_permissions: Option<PermissionProfile>,
        justification: Option<String>,
    },
    ExecCommand {
        id: String,
        command: Vec<String>,
        cwd: PathBuf,
        sandbox_permissions: SandboxPermissions,
        additional_permissions: Option<PermissionProfile>,
        justification: Option<String>,
        tty: bool,
    },
    Execve {
        id: String,
        tool_name: String,
        program: String,
        argv: Vec<String>,
        cwd: PathBuf,
        additional_permissions: Option<PermissionProfile>,
    },
    ApplyPatch {
        id: String,
        cwd: PathBuf,
        files: Vec<AbsolutePathBuf>,
        change_count: usize,
        patch: String,
    },
    NetworkAccess {
        id: String,
        turn_id: String,
        target: String,
        host: String,
        protocol: NetworkApprovalProtocol,
        port: u16,
    },
    McpToolCall {
        id: String,
        server: String,
        tool_name: String,
        arguments: Option<Value>,
        connector_id: Option<String>,
        connector_name: Option<String>,
        connector_description: Option<String>,
        tool_title: Option<String>,
        tool_description: Option<String>,
        annotations: Option<GuardianMcpAnnotations>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GuardianMcpAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
}

#[derive(Serialize)]
struct CommandApprovalAction<'a> {
    tool: &'a str,
    command: &'a [String],
    cwd: &'a PathBuf,
    sandbox_permissions: SandboxPermissions,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_permissions: Option<&'a PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    justification: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tty: Option<bool>,
}

#[derive(Serialize)]
struct ExecveApprovalAction<'a> {
    tool: &'a str,
    program: &'a str,
    argv: &'a [String],
    cwd: &'a PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_permissions: Option<&'a PermissionProfile>,
}

#[derive(Serialize)]
struct McpToolCallApprovalAction<'a> {
    tool: &'static str,
    server: &'a str,
    tool_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector_name: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connector_description: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_title: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_description: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<&'a GuardianMcpAnnotations>,
}

fn serialize_guardian_action(value: impl Serialize) -> serde_json::Result<Value> {
    serde_json::to_value(value)
}

fn serialize_command_guardian_action(
    tool: &'static str,
    command: &[String],
    cwd: &PathBuf,
    sandbox_permissions: SandboxPermissions,
    additional_permissions: Option<&PermissionProfile>,
    justification: Option<&String>,
    tty: Option<bool>,
) -> serde_json::Result<Value> {
    serialize_guardian_action(CommandApprovalAction {
        tool,
        command,
        cwd,
        sandbox_permissions,
        additional_permissions,
        justification,
        tty,
    })
}

fn command_assessment_action_value(tool: &'static str, command: &[String], cwd: &PathBuf) -> Value {
    serde_json::json!({
        "tool": tool,
        "command": chaos_sh::parse_command::shlex_join(command),
        "cwd": cwd,
    })
}

// ---------------------------------------------------------------------------
// Truncation helpers (inlined from chaos-kern guardian/prompt + truncate)
// ---------------------------------------------------------------------------

fn approx_bytes_for_tokens(tokens: usize) -> usize {
    tokens.saturating_mul(APPROX_BYTES_PER_TOKEN)
}

fn approx_tokens_from_byte_count(bytes: usize) -> u64 {
    let bytes_u64 = bytes as u64;
    bytes_u64.saturating_add((APPROX_BYTES_PER_TOKEN as u64).saturating_sub(1))
        / (APPROX_BYTES_PER_TOKEN as u64)
}

fn split_truncation_bounds(
    content: &str,
    prefix_bytes: usize,
    suffix_bytes: usize,
) -> (&str, &str) {
    if content.is_empty() {
        return ("", "");
    }

    let len = content.len();
    let suffix_start_target = len.saturating_sub(suffix_bytes);
    let mut prefix_end = 0usize;
    let mut suffix_start = len;
    let mut suffix_started = false;

    for (index, ch) in content.char_indices() {
        let char_end = index + ch.len_utf8();
        if char_end <= prefix_bytes {
            prefix_end = char_end;
            continue;
        }

        if index >= suffix_start_target {
            if !suffix_started {
                suffix_start = index;
                suffix_started = true;
            }
            continue;
        }
    }

    if suffix_start < prefix_end {
        suffix_start = prefix_end;
    }

    (&content[..prefix_end], &content[suffix_start..])
}

fn guardian_truncate_text(content: &str, token_cap: usize) -> String {
    if content.is_empty() {
        return String::new();
    }

    let max_bytes = approx_bytes_for_tokens(token_cap);
    if content.len() <= max_bytes {
        return content.to_string();
    }

    let omitted_tokens = approx_tokens_from_byte_count(content.len().saturating_sub(max_bytes));
    let marker = format!("<{TRUNCATION_TAG} omitted_approx_tokens=\"{omitted_tokens}\" />");
    if max_bytes <= marker.len() {
        return marker;
    }

    let available_bytes = max_bytes.saturating_sub(marker.len());
    let prefix_budget = available_bytes / 2;
    let suffix_budget = available_bytes.saturating_sub(prefix_budget);
    let (prefix, suffix) = split_truncation_bounds(content, prefix_budget, suffix_budget);

    format!("{prefix}{marker}{suffix}")
}

fn truncate_guardian_action_value(value: Value) -> Value {
    match value {
        Value::String(text) => Value::String(guardian_truncate_text(
            &text,
            GUARDIAN_MAX_ACTION_STRING_TOKENS,
        )),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(truncate_guardian_action_value)
                .collect::<Vec<_>>(),
        ),
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, truncate_guardian_action_value(value)))
                    .collect(),
            )
        }
        other => other,
    }
}

pub fn guardian_approval_request_to_json(
    action: &GuardianApprovalRequest,
) -> serde_json::Result<Value> {
    match action {
        GuardianApprovalRequest::Shell {
            id: _,
            command,
            cwd,
            sandbox_permissions,
            additional_permissions,
            justification,
        } => serialize_command_guardian_action(
            "shell",
            command,
            cwd,
            *sandbox_permissions,
            additional_permissions.as_ref(),
            justification.as_ref(),
            /*tty*/ None,
        ),
        GuardianApprovalRequest::ExecCommand {
            id: _,
            command,
            cwd,
            sandbox_permissions,
            additional_permissions,
            justification,
            tty,
        } => serialize_command_guardian_action(
            "exec_command",
            command,
            cwd,
            *sandbox_permissions,
            additional_permissions.as_ref(),
            justification.as_ref(),
            Some(*tty),
        ),
        GuardianApprovalRequest::Execve {
            id: _,
            tool_name,
            program,
            argv,
            cwd,
            additional_permissions,
        } => serialize_guardian_action(ExecveApprovalAction {
            tool: tool_name,
            program,
            argv,
            cwd,
            additional_permissions: additional_permissions.as_ref(),
        }),
        GuardianApprovalRequest::ApplyPatch {
            id: _,
            cwd,
            files,
            change_count,
            patch,
        } => Ok(serde_json::json!({
            "tool": "apply_patch",
            "cwd": cwd,
            "files": files,
            "change_count": change_count,
            "patch": patch,
        })),
        GuardianApprovalRequest::NetworkAccess {
            id: _,
            turn_id: _,
            target,
            host,
            protocol,
            port,
        } => Ok(serde_json::json!({
            "tool": "network_access",
            "target": target,
            "host": host,
            "protocol": protocol,
            "port": port,
        })),
        GuardianApprovalRequest::McpToolCall {
            id: _,
            server,
            tool_name,
            arguments,
            connector_id,
            connector_name,
            connector_description,
            tool_title,
            tool_description,
            annotations,
        } => serialize_guardian_action(McpToolCallApprovalAction {
            tool: "mcp_tool_call",
            server,
            tool_name,
            arguments: arguments.as_ref(),
            connector_id: connector_id.as_ref(),
            connector_name: connector_name.as_ref(),
            connector_description: connector_description.as_ref(),
            tool_title: tool_title.as_ref(),
            tool_description: tool_description.as_ref(),
            annotations: annotations.as_ref(),
        }),
    }
}

pub fn guardian_assessment_action_value(action: &GuardianApprovalRequest) -> Value {
    match action {
        GuardianApprovalRequest::Shell { command, cwd, .. } => {
            command_assessment_action_value("shell", command, cwd)
        }
        GuardianApprovalRequest::ExecCommand { command, cwd, .. } => {
            command_assessment_action_value("exec_command", command, cwd)
        }
        GuardianApprovalRequest::Execve {
            tool_name,
            program,
            argv,
            cwd,
            ..
        } => serde_json::json!({
            "tool": tool_name,
            "program": program,
            "argv": argv,
            "cwd": cwd,
        }),
        GuardianApprovalRequest::ApplyPatch {
            cwd,
            files,
            change_count,
            ..
        } => serde_json::json!({
            "tool": "apply_patch",
            "cwd": cwd,
            "files": files,
            "change_count": change_count,
        }),
        GuardianApprovalRequest::NetworkAccess {
            id: _,
            turn_id: _,
            target,
            host,
            protocol,
            port,
        } => serde_json::json!({
            "tool": "network_access",
            "target": target,
            "host": host,
            "protocol": protocol,
            "port": port,
        }),
        GuardianApprovalRequest::McpToolCall {
            server, tool_name, ..
        } => serde_json::json!({
            "tool": "mcp_tool_call",
            "server": server,
            "tool_name": tool_name,
        }),
    }
}

pub fn guardian_request_id(request: &GuardianApprovalRequest) -> &str {
    match request {
        GuardianApprovalRequest::Shell { id, .. }
        | GuardianApprovalRequest::ExecCommand { id, .. }
        | GuardianApprovalRequest::ApplyPatch { id, .. }
        | GuardianApprovalRequest::NetworkAccess { id, .. }
        | GuardianApprovalRequest::McpToolCall { id, .. } => id,
        GuardianApprovalRequest::Execve { id, .. } => id,
    }
}

pub fn guardian_request_turn_id<'a>(
    request: &'a GuardianApprovalRequest,
    default_turn_id: &'a str,
) -> &'a str {
    match request {
        GuardianApprovalRequest::NetworkAccess { turn_id, .. } => turn_id,
        GuardianApprovalRequest::Shell { .. }
        | GuardianApprovalRequest::ExecCommand { .. }
        | GuardianApprovalRequest::ApplyPatch { .. }
        | GuardianApprovalRequest::McpToolCall { .. } => default_turn_id,
        GuardianApprovalRequest::Execve { .. } => default_turn_id,
    }
}

pub fn format_guardian_action_pretty(
    action: &GuardianApprovalRequest,
) -> serde_json::Result<String> {
    let mut value = guardian_approval_request_to_json(action)?;
    value = truncate_guardian_action_value(value);
    serde_json::to_string_pretty(&value)
}
