//! Clamp tool routing, permission dispatching, and response rendering.

use std::sync::Weak;

use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FileSystemPermissions;
use chaos_ipc::models::PermissionProfile;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::permissions::FileSystemSandboxPolicy;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::WarningEvent;
use chaos_parole::sandbox::can_read_path;
use chaos_parole::sandbox::can_write_path;
use chaos_realpath::AbsolutePathBuf;
use serde_json::Value;

use crate::client_common::Prompt;
use crate::exec_policy::ExecApprovalRequest;

pub(crate) const CLAMP_NATIVE_PASSTHROUGH_TOOLS: &[&str] = &["WebSearch", "WebFetch"];

pub(crate) fn clamp_permission_mode(approval_policy: ApprovalPolicy) -> String {
    match approval_policy {
        ApprovalPolicy::Headless => "bypassPermissions",
        ApprovalPolicy::Supervised | ApprovalPolicy::Interactive | ApprovalPolicy::Granular(_) => {
            "default"
        }
    }
    .to_string()
}

pub(crate) fn build_clamp_mcp_config(socket_path: &std::path::Path, token: &str) -> Value {
    let command = std::env::current_exe()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.is_empty())
        .unwrap_or_else(|| "chaos".to_string());
    serde_json::json!({
        "mcpServers": {
            "chaos": {
                "command": command,
                "args": ["clamp-session-bridge"],
                "env": {
                    "CHAOS_CLAMP_MCP_SOCKET": socket_path.to_string_lossy(),
                    "CHAOS_CLAMP_MCP_TOKEN": token
                }
            }
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClampToolPermissionDecision {
    Allow,
    AskPermissions {
        permissions: PermissionProfile,
        reason: String,
    },
    AskCommandApproval {
        command: Vec<String>,
        reason: String,
    },
    Deny(String),
}

#[cfg_attr(test, allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClampLocalToolKind {
    Shell,
    FsRead,
    FsWrite,
    FsReadPathOptional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClampToolRouting {
    Passthrough,
    Local {
        local_tool_name: &'static str,
        kind: ClampLocalToolKind,
    },
}

pub(crate) fn clamp_tool_routing(tool_name: &str) -> Option<ClampToolRouting> {
    if CLAMP_NATIVE_PASSTHROUGH_TOOLS.contains(&tool_name) {
        return Some(ClampToolRouting::Passthrough);
    }

    match tool_name {
        "Bash" => Some(ClampToolRouting::Local {
            local_tool_name: "exec_command",
            kind: ClampLocalToolKind::Shell,
        }),
        "Read" => Some(ClampToolRouting::Local {
            local_tool_name: "read_file",
            kind: ClampLocalToolKind::FsRead,
        }),
        "NotebookRead" => Some(ClampToolRouting::Local {
            local_tool_name: "read_file",
            kind: ClampLocalToolKind::FsRead,
        }),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => Some(ClampToolRouting::Local {
            local_tool_name: "apply_patch",
            kind: ClampLocalToolKind::FsWrite,
        }),
        "Glob" | "Grep" | "LS" => Some(ClampToolRouting::Local {
            local_tool_name: "read_file",
            kind: ClampLocalToolKind::FsReadPathOptional,
        }),
        _ => None,
    }
}

pub(crate) fn clamp_permission_allow_response(input: Value) -> Value {
    serde_json::json!({
        "behavior": "allow",
        "updatedInput": input
    })
}

pub(crate) fn clamp_permission_deny_response(message: impl Into<String>) -> Value {
    serde_json::json!({
        "behavior": "deny",
        "message": message.into()
    })
}

fn clamp_resolve_input_path(
    input: &Value,
    cwd: &std::path::Path,
    keys: &[&str],
) -> Option<AbsolutePathBuf> {
    let object = input.as_object()?;
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(|value| value.as_str())
        .and_then(|path| AbsolutePathBuf::resolve_path_against_base(path, cwd).ok())
}

fn clamp_read_permission(path: AbsolutePathBuf) -> PermissionProfile {
    PermissionProfile {
        network: None,
        file_system: Some(FileSystemPermissions {
            read: Some(vec![path]),
            write: None,
        }),
        macos: None,
    }
}

fn clamp_write_permission(path: AbsolutePathBuf) -> PermissionProfile {
    PermissionProfile {
        network: None,
        file_system: Some(FileSystemPermissions {
            read: None,
            write: Some(vec![path]),
        }),
        macos: None,
    }
}

pub(crate) fn clamp_effective_file_system_policy(
    turn: &crate::chaos::TurnContext,
    granted_permissions: Option<&PermissionProfile>,
) -> FileSystemSandboxPolicy {
    crate::sandboxing::effective_file_system_sandbox_policy(
        &turn.file_system_sandbox_policy,
        granted_permissions,
    )
}

pub(crate) fn clamp_tool_permission_decision(
    tool_name: &str,
    input: &Value,
    cwd: &std::path::Path,
    file_system_policy: &FileSystemSandboxPolicy,
) -> ClampToolPermissionDecision {
    let Some(routing) = clamp_tool_routing(tool_name) else {
        return ClampToolPermissionDecision::Deny(format!(
            "Claude Code built-in tool '{tool_name}' is not supported in clamp mode; use Chaos-managed tools instead."
        ));
    };

    match routing {
        ClampToolRouting::Passthrough => ClampToolPermissionDecision::Allow,
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::Shell,
        } => {
            let command = input
                .get("command")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty());
            match command {
                Some(command) => ClampToolPermissionDecision::AskCommandApproval {
                    command: vec![
                        "/bin/sh".to_string(),
                        "-lc".to_string(),
                        command.to_string(),
                    ],
                    reason: format!(
                        "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests permission to run a shell command."
                    ),
                },
                None => ClampToolPermissionDecision::Deny(format!(
                    "Claude Code {tool_name} request is missing a command."
                )),
            }
        }
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::FsRead,
        } => match clamp_resolve_input_path(input, cwd, &["file_path", "path"]) {
            Some(path) if can_read_path(file_system_policy, path.as_path(), cwd) => {
                ClampToolPermissionDecision::Allow
            }
            Some(path) => ClampToolPermissionDecision::AskPermissions {
                permissions: clamp_read_permission(path),
                reason: format!(
                    "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests filesystem read access."
                ),
            },
            None => ClampToolPermissionDecision::Deny(format!(
                "Claude Code {tool_name} request is missing a readable path."
            )),
        },
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::FsWrite,
        } => match clamp_resolve_input_path(input, cwd, &["file_path", "path"]) {
            Some(path) if can_write_path(file_system_policy, path.as_path(), cwd) => {
                ClampToolPermissionDecision::Allow
            }
            Some(path) => ClampToolPermissionDecision::AskPermissions {
                permissions: clamp_write_permission(path),
                reason: format!(
                    "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests filesystem write access."
                ),
            },
            None => ClampToolPermissionDecision::Deny(format!(
                "Claude Code {tool_name} request is missing a writable path."
            )),
        },
        ClampToolRouting::Local {
            local_tool_name,
            kind: ClampLocalToolKind::FsReadPathOptional,
        } => match clamp_resolve_input_path(input, cwd, &["path"]) {
            Some(path) if can_read_path(file_system_policy, path.as_path(), cwd) => {
                ClampToolPermissionDecision::Allow
            }
            Some(path) => ClampToolPermissionDecision::AskPermissions {
                permissions: clamp_read_permission(path),
                reason: format!(
                    "Claude Code {tool_name} routes through local tool '{local_tool_name}' and requests filesystem read access."
                ),
            },
            None => ClampToolPermissionDecision::Allow,
        },
    }
}

pub(crate) async fn active_clamp_turn_context(
    session: &crate::chaos::Session,
) -> Option<std::sync::Arc<crate::chaos::TurnContext>> {
    let active = session.active_turn.lock().await;
    let (_, task) = active.as_ref()?.tasks.first()?;
    Some(std::sync::Arc::clone(&task.turn_context))
}

pub(crate) async fn handle_clamp_mcp_message(
    session: Weak<crate::chaos::Session>,
    server_name: String,
    message: Value,
) -> std::result::Result<Value, String> {
    let Some(session) = session.upgrade() else {
        return Err("session closed".to_string());
    };

    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing MCP method".to_string())?;

    match method {
        "tools/call" => {
            let params = message
                .get("params")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "missing MCP params".to_string())?;
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing MCP tool name".to_string())?
                .to_string();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let raw_arguments = serde_json::to_string(&arguments)
                .map_err(|err| format!("failed to serialize MCP arguments: {err}"))?;
            let turn_context = active_clamp_turn_context(&session)
                .await
                .ok_or_else(|| "no active turn for clamp MCP tool call".to_string())?;
            let call_id = format!("clamp_mcp_{}", uuid::Uuid::now_v7());
            let result = crate::mcp_tool_call::handle_mcp_tool_call(
                std::sync::Arc::clone(&session),
                &turn_context,
                call_id,
                server_name,
                tool_name,
                raw_arguments,
            )
            .await;
            let result_value = serde_json::to_value(&result)
                .map_err(|err| format!("failed to serialize MCP result: {err}"))?;
            Ok(serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result_value
            }))
        }
        _ => Ok(serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("unsupported clamp MCP method: {method}")
            }
        })),
    }
}

pub(crate) async fn handle_clamp_tool_permission(
    session: Weak<crate::chaos::Session>,
    tool_name: String,
    input: Value,
    tool_use_id: Option<String>,
) -> std::result::Result<Value, String> {
    use chaos_ipc::request_permissions::RequestPermissionProfile;
    use chaos_ipc::request_permissions::RequestPermissionsArgs;

    let Some(session) = session.upgrade() else {
        return Err("session closed".to_string());
    };
    let turn_context = active_clamp_turn_context(&session)
        .await
        .ok_or_else(|| "no active turn for clamp tool permission".to_string())?;
    let granted_permissions = crate::sandboxing::merge_permission_profiles(
        session.granted_session_permissions().await.as_ref(),
        session.granted_turn_permissions().await.as_ref(),
    );
    let file_system_policy =
        clamp_effective_file_system_policy(&turn_context, granted_permissions.as_ref());
    let decision = clamp_tool_permission_decision(
        &tool_name,
        &input,
        turn_context.cwd.as_path(),
        &file_system_policy,
    );
    let call_id = tool_use_id.unwrap_or_else(|| format!("clamp_tool_{}", uuid::Uuid::now_v7()));

    match decision {
        ClampToolPermissionDecision::Allow => Ok(clamp_permission_allow_response(input)),
        ClampToolPermissionDecision::Deny(message) => Ok(clamp_permission_deny_response(message)),
        ClampToolPermissionDecision::AskPermissions {
            permissions,
            reason,
        } => {
            let response = session
                .request_permissions(
                    turn_context.as_ref(),
                    call_id,
                    RequestPermissionsArgs {
                        reason: Some(reason.clone()),
                        permissions: RequestPermissionProfile::from(permissions.clone()),
                    },
                )
                .await
                .ok_or_else(|| "clamp permission request cancelled".to_string())?;
            let granted = crate::sandboxing::intersect_permission_profiles(
                permissions.clone(),
                response.permissions.into(),
            );
            if granted == permissions {
                Ok(clamp_permission_allow_response(input))
            } else {
                Ok(clamp_permission_deny_response(format!(
                    "{reason} Access was not granted."
                )))
            }
        }
        ClampToolPermissionDecision::AskCommandApproval { command, reason } => {
            let exec_approval_requirement = session
                .services
                .exec_policy
                .create_exec_approval_requirement_for_command(ExecApprovalRequest {
                    command: &command,
                    approval_policy: turn_context.approval_policy.value(),
                    file_system_sandbox_policy: &turn_context.file_system_sandbox_policy,
                    sandbox_permissions: chaos_ipc::models::SandboxPermissions::UseDefault,
                    prefix_rule: None,
                })
                .await;
            match exec_approval_requirement {
                crate::tools::sandboxing::ExecApprovalRequirement::Skip { .. } => {
                    Ok(clamp_permission_allow_response(input))
                }
                crate::tools::sandboxing::ExecApprovalRequirement::Forbidden { reason } => {
                    Ok(clamp_permission_deny_response(reason))
                }
                crate::tools::sandboxing::ExecApprovalRequirement::NeedsApproval {
                    reason: approval_reason,
                    proposed_execpolicy_amendment,
                } => {
                    let review_decision = session
                        .request_command_approval(
                            turn_context.as_ref(),
                            call_id,
                            None,
                            command,
                            turn_context.cwd.clone(),
                            approval_reason.or(Some(reason)),
                            None,
                            proposed_execpolicy_amendment,
                            None,
                            None,
                        )
                        .await;
                    if matches!(
                        review_decision,
                        chaos_ipc::protocol::ReviewDecision::Approved
                            | chaos_ipc::protocol::ReviewDecision::ApprovedForSession
                    ) {
                        Ok(clamp_permission_allow_response(input))
                    } else {
                        Ok(clamp_permission_deny_response(
                            "Command execution was not approved.",
                        ))
                    }
                }
            }
        }
    }
}

pub(crate) async fn handle_clamp_hook_callback(
    session: Weak<crate::chaos::Session>,
    callback_id: String,
    _input: Value,
    tool_use_id: Option<String>,
) -> std::result::Result<Value, String> {
    let Some(session) = session.upgrade() else {
        return Err("session closed".to_string());
    };
    if let Some(turn_context) = active_clamp_turn_context(&session).await {
        session
            .send_event(
                turn_context.as_ref(),
                EventMsg::Warning(WarningEvent {
                    message: format!(
                        "Clamp received unexpected Claude hook callback '{}'{}; clamp sessions do not currently register callback hooks.",
                        callback_id,
                        tool_use_id
                            .as_deref()
                            .map(|id| format!(" (tool_use_id: {id})"))
                            .unwrap_or_default()
                    ),
                }),
            )
            .await;
    }
    Ok(serde_json::json!({}))
}

pub(crate) fn render_clamp_content_items(content: &[ContentItem]) -> String {
    content
        .iter()
        .map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => text.clone(),
            ContentItem::InputImage { image_url } => {
                if image_url.starts_with("data:") {
                    "[image: inline data omitted]".to_string()
                } else {
                    format!("[image: {image_url}]")
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn render_json_pretty<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|err| format!("<serialization error: {err}>"))
}

pub(crate) fn clamp_elide_large_text(text: &str) -> String {
    const MAX_CHARS: usize = 8_000;
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        format!(
            "{preview}\n...[truncated {} chars]",
            text.chars().count() - MAX_CHARS
        )
    } else {
        preview
    }
}

pub(crate) fn render_clamp_response_item(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { role, content, .. } => Some(format!(
            "<message role=\"{role}\">\n{}\n</message>",
            render_clamp_content_items(content)
        )),
        ResponseItem::Reasoning { summary, .. } => {
            let text = summary
                .iter()
                .map(|entry| match entry {
                    chaos_ipc::models::ReasoningItemReasoningSummary::SummaryText { text } => {
                        text.as_str()
                    }
                })
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then(|| format!("<reasoning_summary>\n{text}\n</reasoning_summary>"))
        }
        ResponseItem::LocalShellCall {
            call_id,
            status,
            action,
            ..
        } => Some(format!(
            "<local_shell_call call_id=\"{}\" status=\"{}\">\n{}\n</local_shell_call>",
            call_id.as_deref().unwrap_or(""),
            serde_json::to_string(status).unwrap_or_else(|_| "\"unknown\"".to_string()),
            render_json_pretty(action)
        )),
        ResponseItem::FunctionCall {
            name,
            call_id,
            arguments,
            namespace,
            ..
        } => Some(format!(
            "<function_call name=\"{name}\" namespace=\"{}\" call_id=\"{call_id}\">\n{}\n</function_call>",
            namespace.as_deref().unwrap_or(""),
            arguments
        )),
        ResponseItem::ToolSearchCall {
            call_id,
            status,
            execution,
            arguments,
            ..
        } => Some(format!(
            "<tool_search_call call_id=\"{}\" status=\"{}\" execution=\"{execution}\">\n{}\n</tool_search_call>",
            call_id.as_deref().unwrap_or(""),
            status.as_deref().unwrap_or(""),
            render_json_pretty(arguments)
        )),
        ResponseItem::FunctionCallOutput {
            call_id, output, ..
        }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => Some(format!(
            "<tool_output call_id=\"{call_id}\">\n{}\n</tool_output>",
            clamp_elide_large_text(
                &output
                    .body
                    .to_text()
                    .unwrap_or_else(|| render_json_pretty(output))
            )
        )),
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            status,
            ..
        } => Some(format!(
            "<custom_tool_call name=\"{name}\" call_id=\"{call_id}\" status=\"{}\">\n{input}\n</custom_tool_call>",
            status.as_deref().unwrap_or("")
        )),
        ResponseItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => Some(format!(
            "<tool_search_output call_id=\"{}\" status=\"{status}\" execution=\"{execution}\">\n{}\n</tool_search_output>",
            call_id.as_deref().unwrap_or(""),
            render_json_pretty(tools)
        )),
        ResponseItem::WebSearchCall { status, action, .. } => Some(format!(
            "<web_search_call status=\"{}\">\n{}\n</web_search_call>",
            status.as_deref().unwrap_or(""),
            action.as_ref().map(render_json_pretty).unwrap_or_default()
        )),
        ResponseItem::ImageGenerationCall {
            status,
            revised_prompt,
            result,
            ..
        } => Some(format!(
            "<image_generation_call status=\"{status}\">\nrevised_prompt: {}\nresult: {}\n</image_generation_call>",
            revised_prompt.as_deref().unwrap_or(""),
            clamp_elide_large_text(result)
        )),
        ResponseItem::GhostSnapshot { .. } => {
            Some("<ghost_snapshot>[omitted]</ghost_snapshot>".to_string())
        }
        ResponseItem::Compaction { .. } => Some("<compaction>[omitted]</compaction>".to_string()),
        ResponseItem::Other => Some("<other_response_item />".to_string()),
    }
}

pub(crate) fn render_clamp_full_prompt(prompt: &Prompt) -> String {
    let rendered_items = prompt
        .get_formatted_input()
        .iter()
        .filter_map(render_clamp_response_item)
        .collect::<Vec<_>>();

    if rendered_items.is_empty() {
        return "Chaos restored an empty conversation state. Respond to the latest user request."
            .to_string();
    }

    format!(
        "Chaos restored the current Chaos conversation state after connecting Claude Code.\n\
Treat the transcript below as authoritative prior context, including tool calls and tool outputs that already happened.\n\
Continue from the latest user request instead of restarting the conversation.\n\n\
<conversation_state>\n{}\n</conversation_state>",
        rendered_items.join("\n\n")
    )
}

pub(crate) fn render_latest_clamp_user_message(prompt: &Prompt) -> String {
    prompt
        .get_formatted_input()
        .iter()
        .rev()
        .find_map(|item| match item {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                let rendered = render_clamp_content_items(content);
                (!rendered.is_empty()).then_some(rendered)
            }
            _ => None,
        })
        .unwrap_or_else(|| render_clamp_full_prompt(prompt))
}
