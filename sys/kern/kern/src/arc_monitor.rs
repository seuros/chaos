use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use tracing::warn;

use crate::chaos::Session;
use crate::chaos::TurnContext;
use crate::default_client::build_http_client;
use crate::distill::content_items_to_text;
use crate::env::read_non_empty_env_var;
use crate::event_mapping::is_contextual_user_message_content;
use chaos_ipc::models::MessagePhase;
use chaos_ipc::models::ResponseItem;

const ARC_MONITOR_TIMEOUT: Duration = Duration::from_secs(30);
const CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE: &str = "CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE";
const CHAOS_ARC_MONITOR_TOKEN: &str = "CHAOS_ARC_MONITOR_TOKEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ArcMonitorOutcome {
    Disabled,
    Ok,
    SteerModel(String),
    AskUser(String),
    Unavailable(&'static str),
}

pub(crate) fn monitoring_required(config: &crate::config::Config) -> bool {
    crate::reflex::action_risk_settings(config).is_some()
        || std::env::var_os(CHAOS_ARC_MONITOR_TOKEN).is_some()
        || std::env::var_os(CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE).is_some()
}

#[derive(Debug, Serialize, PartialEq)]
struct ArcMonitorRequest {
    metadata: ArcMonitorMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    messages: Option<Vec<ArcMonitorChatMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Vec<ResponseItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    policies: Option<ArcMonitorPolicies>,
    action: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArcMonitorResult {
    outcome: ArcMonitorResultOutcome,
    short_reason: String,
    rationale: String,
    risk_score: u8,
    risk_level: ArcMonitorRiskLevel,
    evidence: Vec<ArcMonitorEvidence>,
}

#[derive(Debug, Serialize, PartialEq)]
struct ArcMonitorChatMessage {
    role: String,
    content: serde_json::Value,
}

#[derive(Debug, Serialize, PartialEq)]
struct ArcMonitorPolicies {
    user: Option<String>,
    developer: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ArcMonitorMetadata {
    codex_process_id: String,
    codex_turn_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    protection_client_callsite: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ArcMonitorEvidence {
    message: String,
    why: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ArcMonitorResultOutcome {
    Ok,
    SteerModel,
    AskUser,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ArcMonitorRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

pub(crate) async fn monitor_action(
    sess: &Session,
    turn_context: &TurnContext,
    action: serde_json::Value,
) -> ArcMonitorOutcome {
    let outcome = tokio::time::timeout(
        ARC_MONITOR_TIMEOUT,
        monitor_action_inner(sess, turn_context, action),
    )
    .await
    .unwrap_or(ArcMonitorOutcome::Unavailable("deadline"));
    if let ArcMonitorOutcome::Unavailable(category) = &outcome {
        warn!(error_kind = category, "safety check unavailable");
    }
    outcome
}

async fn monitor_action_inner(
    sess: &Session,
    turn_context: &TurnContext,
    action: serde_json::Value,
) -> ArcMonitorOutcome {
    let serde_json::Value::Object(action) = action else {
        return ArcMonitorOutcome::Unavailable("invalid_action");
    };
    if let Some((_, settings)) = crate::reflex::action_risk_settings(&turn_context.config) {
        let config = turn_context.config.clone();
        let auth = turn_context.auth_manager.clone();
        let initialized = tokio::task::spawn_blocking(move || {
            crate::reflex::from_config(&config, auth.as_deref())
        })
        .await;
        let outcome = match initialized {
            Ok(Ok(Some(reflex))) => {
                let history = sess.clone_history().await;
                let conversation = build_arc_monitor_messages(history.raw_items())
                    .iter()
                    .filter_map(|message| serde_json::to_value(message).ok())
                    .collect();
                let instructions = crate::reflex::joined_instructions(
                    turn_context.developer_instructions.as_deref(),
                    turn_context.user_instructions.as_deref(),
                );
                crate::reflex::assess_action(
                    &reflex,
                    conversation,
                    serde_json::Value::Object(action.clone()),
                    instructions,
                )
                .await
            }
            Ok(Err(reason)) => ArcMonitorOutcome::Unavailable(reason),
            _ => ArcMonitorOutcome::Unavailable("backend_initialization"),
        };
        if !matches!(outcome, ArcMonitorOutcome::Unavailable(_)) || !settings.allow_remote_fallback
        {
            return outcome;
        }
    }
    let auth = match turn_context.auth_manager.as_ref() {
        Some(auth_manager) => match auth_manager.auth().await {
            Some(auth) if auth.is_chatgpt_auth() => Some(auth),
            _ => None,
        },
        None => None,
    };
    let token = if let Some(token) = read_non_empty_env_var(CHAOS_ARC_MONITOR_TOKEN) {
        token
    } else {
        let Some(auth) = auth.as_ref() else {
            return if monitoring_required(&turn_context.config) {
                ArcMonitorOutcome::Unavailable("remote_credentials")
            } else {
                ArcMonitorOutcome::Disabled
            };
        };
        match auth.get_token() {
            Ok(token) => token,
            Err(_) => {
                warn!("safety monitor auth token unavailable");
                return ArcMonitorOutcome::Unavailable("remote_credentials");
            }
        }
    };

    let url = read_non_empty_env_var(CHAOS_ARC_MONITOR_ENDPOINT_OVERRIDE).unwrap_or_else(|| {
        format!(
            "{}/chaos/safety/arc",
            turn_context.config.chatgpt_base_url.trim_end_matches('/')
        )
    });
    let body = build_arc_monitor_request(sess, turn_context, action).await;
    let client = build_http_client();
    let mut request = client
        .post(&url)
        .timeout(ARC_MONITOR_TIMEOUT)
        .json(&body)
        .bearer_auth(token);
    if let Some(account_id) = auth
        .as_ref()
        .and_then(crate::auth::ChaosAuth::get_account_id)
    {
        request = request.header("chatgpt-account-id", account_id);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(_) => {
            warn!("safety monitor request failed");
            return ArcMonitorOutcome::Unavailable("remote_transport");
        }
    };
    let status = response.status();
    if !status.is_success() {
        warn!(%status, "safety monitor returned non-success status");
        return ArcMonitorOutcome::Unavailable("remote_http");
    }

    let response = match response.json::<ArcMonitorResult>().await {
        Ok(response) => response,
        Err(_) => {
            warn!("failed to parse safety monitor response");
            return ArcMonitorOutcome::Unavailable("remote_malformed");
        }
    };
    if response.risk_score > 100 {
        return ArcMonitorOutcome::Unavailable("remote_malformed");
    }
    tracing::debug!(
        risk_score = response.risk_score,
        risk_level = ?response.risk_level,
        evidence_count = response.evidence.len(),
        "safety monitor completed"
    );

    let short_reason = response.short_reason.trim();
    let rationale = response.rationale.trim();
    match response.outcome {
        ArcMonitorResultOutcome::Ok => ArcMonitorOutcome::Ok,
        ArcMonitorResultOutcome::AskUser => {
            if !short_reason.is_empty() {
                ArcMonitorOutcome::AskUser(short_reason.to_string())
            } else if !rationale.is_empty() {
                ArcMonitorOutcome::AskUser(rationale.to_string())
            } else {
                ArcMonitorOutcome::AskUser(
                    "Additional confirmation is required before this tool call can continue."
                        .to_string(),
                )
            }
        }
        ArcMonitorResultOutcome::SteerModel => {
            if !rationale.is_empty() {
                ArcMonitorOutcome::SteerModel(rationale.to_string())
            } else if !short_reason.is_empty() {
                ArcMonitorOutcome::SteerModel(short_reason.to_string())
            } else {
                ArcMonitorOutcome::SteerModel(
                    "Tool call was cancelled because of safety risks.".to_string(),
                )
            }
        }
    }
}

async fn build_arc_monitor_request(
    sess: &Session,
    turn_context: &TurnContext,
    action: serde_json::Map<String, serde_json::Value>,
) -> ArcMonitorRequest {
    let history = sess.clone_history().await;
    let mut messages = build_arc_monitor_messages(history.raw_items());
    if messages.is_empty() {
        messages.push(build_arc_monitor_message(
            "user",
            serde_json::Value::String(
                "No prior conversation history is available for this ARC evaluation.".to_string(),
            ),
        ));
    }

    let conversation_id = sess.conversation_id.to_string();
    ArcMonitorRequest {
        metadata: ArcMonitorMetadata {
            codex_process_id: conversation_id.clone(),
            codex_turn_id: turn_context.sub_id.clone(),
            conversation_id: Some(conversation_id),
            protection_client_callsite: None,
        },
        messages: Some(messages),
        input: None,
        policies: Some(ArcMonitorPolicies {
            user: None,
            developer: None,
        }),
        action,
    }
}

fn build_arc_monitor_messages(items: &[ResponseItem]) -> Vec<ArcMonitorChatMessage> {
    let last_tool_call_index = items
        .iter()
        .enumerate()
        .rev()
        .find(|(_, item)| {
            matches!(
                item,
                ResponseItem::LocalShellCall { .. }
                    | ResponseItem::FunctionCall { .. }
                    | ResponseItem::CustomToolCall { .. }
                    | ResponseItem::WebSearchCall { .. }
            )
        })
        .map(|(index, _)| index);
    let last_encrypted_reasoning_index = items
        .iter()
        .enumerate()
        .rev()
        .find(|(_, item)| {
            matches!(
                item,
                ResponseItem::Reasoning {
                    encrypted_content: Some(encrypted_content),
                    ..
                } if !encrypted_content.trim().is_empty()
            )
        })
        .map(|(index, _)| index);

    items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            build_arc_monitor_message_item(
                item,
                index,
                last_tool_call_index,
                last_encrypted_reasoning_index,
            )
        })
        .collect()
}

fn build_arc_monitor_message_item(
    item: &ResponseItem,
    index: usize,
    last_tool_call_index: Option<usize>,
    last_encrypted_reasoning_index: Option<usize>,
) -> Option<ArcMonitorChatMessage> {
    match item {
        ResponseItem::Message { role, content, .. } if role == "user" => {
            if is_contextual_user_message_content(content) {
                None
            } else {
                content_items_to_text(content)
                    .map(|text| build_arc_monitor_text_message("user", "input_text", text))
            }
        }
        ResponseItem::Message {
            role,
            content,
            phase: Some(MessagePhase::FinalAnswer),
            ..
        } if role == "assistant" => content_items_to_text(content)
            .map(|text| build_arc_monitor_text_message("assistant", "output_text", text)),
        ResponseItem::Message { .. } => None,
        ResponseItem::Reasoning {
            encrypted_content: Some(encrypted_content),
            ..
        } if Some(index) == last_encrypted_reasoning_index
            && !encrypted_content.trim().is_empty() =>
        {
            Some(build_arc_monitor_message(
                "assistant",
                serde_json::json!([{
                    "type": "encrypted_reasoning",
                    "encrypted_content": encrypted_content,
                }]),
            ))
        }
        ResponseItem::Reasoning { .. } => None,
        ResponseItem::LocalShellCall { action, .. } if Some(index) == last_tool_call_index => {
            Some(build_arc_monitor_message(
                "assistant",
                serde_json::json!([{
                    "type": "tool_call",
                    "tool_name": "shell",
                    "action": action,
                }]),
            ))
        }
        ResponseItem::FunctionCall {
            name, arguments, ..
        } if Some(index) == last_tool_call_index => Some(build_arc_monitor_message(
            "assistant",
            serde_json::json!([{
                "type": "tool_call",
                "tool_name": name,
                "arguments": arguments,
            }]),
        )),
        ResponseItem::CustomToolCall { name, input, .. } if Some(index) == last_tool_call_index => {
            Some(build_arc_monitor_message(
                "assistant",
                serde_json::json!([{
                    "type": "tool_call",
                    "tool_name": name,
                    "input": input,
                }]),
            ))
        }
        ResponseItem::WebSearchCall { action, .. } if Some(index) == last_tool_call_index => {
            Some(build_arc_monitor_message(
                "assistant",
                serde_json::json!([{
                    "type": "tool_call",
                    "tool_name": "web_search",
                    "action": action,
                }]),
            ))
        }
        ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::CompactionTrigger {}
        | ResponseItem::Other => None,
    }
}

fn build_arc_monitor_text_message(
    role: &str,
    part_type: &str,
    text: String,
) -> ArcMonitorChatMessage {
    build_arc_monitor_message(
        role,
        serde_json::json!([{
            "type": part_type,
            "text": text,
        }]),
    )
}

fn build_arc_monitor_message(role: &str, content: serde_json::Value) -> ArcMonitorChatMessage {
    ArcMonitorChatMessage {
        role: role.to_string(),
        content,
    }
}

#[cfg(test)]
#[path = "arc_monitor_tests.rs"]
mod tests;
