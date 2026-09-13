use crate::model::ProcessMetadata;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::permissions::VfsPolicyKind;
use chaos_ipc::protocol::ENVIRONMENT_CONTEXT_CLOSE_TAG;
use chaos_ipc::protocol::ENVIRONMENT_CONTEXT_OPEN_TAG;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SessionMetaLine;
use chaos_ipc::protocol::TurnContextItem;
use chaos_ipc::protocol::USER_MESSAGE_BEGIN;
use chaos_ipc::protocol::UserMessageEvent;
use serde::Serialize;
use serde_json::Value;

const IMAGE_ONLY_USER_MESSAGE_PLACEHOLDER: &str = "[Image]";

/// Apply a rollout item to the metadata structure.
pub fn apply_rollout_item(
    metadata: &mut ProcessMetadata,
    item: &RolloutItem,
    default_provider: &str,
) {
    match item {
        RolloutItem::SessionMeta(meta_line) => apply_session_meta_from_item(metadata, meta_line),
        RolloutItem::TurnContext(turn_ctx) => apply_turn_context(metadata, turn_ctx),
        RolloutItem::EventMsg(event) => apply_event_msg(metadata, event),
        RolloutItem::ResponseItem(item) => apply_response_item(metadata, item),
        RolloutItem::BackgroundTask(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::CompactionControl(_) => {}
    }
    if metadata.model_provider.is_empty() {
        metadata.model_provider = default_provider.to_string();
    }
}

/// Return whether this rollout item can mutate persisted process metadata.
pub fn rollout_item_affects_process_metadata(item: &RolloutItem) -> bool {
    match item {
        RolloutItem::SessionMeta(_) | RolloutItem::TurnContext(_) => true,
        RolloutItem::EventMsg(EventMsg::TokenCount(_) | EventMsg::UserMessage(_)) => true,
        RolloutItem::ResponseItem(ResponseItem::Message { role, .. }) => role == "user",
        RolloutItem::BackgroundTask(_)
        | RolloutItem::EventMsg(_)
        | RolloutItem::ResponseItem(_)
        | RolloutItem::Compacted(_)
        | RolloutItem::CompactionControl(_) => false,
    }
}

fn apply_session_meta_from_item(metadata: &mut ProcessMetadata, meta_line: &SessionMetaLine) {
    if metadata.id != meta_line.meta.id {
        // Ignore session_meta lines that don't match the canonical thread ID,
        // e.g., forked rollouts that embed the source session metadata.
        return;
    }
    metadata.id = meta_line.meta.id;
    metadata.source = enum_to_string(&meta_line.meta.source);
    metadata.agent_nickname = meta_line.meta.agent_nickname.clone();
    metadata.agent_role = meta_line.meta.agent_role.clone();
    if let Some(provider) = meta_line.meta.model_provider.as_deref() {
        metadata.model_provider = provider.to_string();
    }
    if !meta_line.meta.cli_version.is_empty() {
        metadata.cli_version = meta_line.meta.cli_version.clone();
    }
    if !meta_line.meta.cwd.as_os_str().is_empty() {
        metadata.cwd = meta_line.meta.cwd.clone();
    }
    if let Some(git) = meta_line.git.as_ref() {
        metadata.git_sha = git.commit_hash.clone();
        metadata.git_branch = git.branch.clone();
        metadata.git_origin_url = git.repository_url.clone();
    }
}

fn apply_turn_context(metadata: &mut ProcessMetadata, turn_ctx: &TurnContextItem) {
    if metadata.cwd.as_os_str().is_empty() {
        metadata.cwd = turn_ctx.cwd.clone();
    }
    metadata.sandbox_policy = sandbox_policy_label(turn_ctx);
    metadata.approval_mode = enum_to_string(&turn_ctx.approval_policy);
}

fn sandbox_policy_label(turn_ctx: &TurnContextItem) -> String {
    match turn_ctx.vfs_policy.kind {
        VfsPolicyKind::ExternalSandbox => "external-sandbox".to_string(),
        VfsPolicyKind::Unrestricted => "root-access".to_string(),
        VfsPolicyKind::Restricted => {
            if turn_ctx.vfs_policy.has_full_disk_write_access() {
                if turn_ctx.socket_policy.is_enabled() {
                    "root-access".to_string()
                } else {
                    "workspace-write".to_string()
                }
            } else if turn_ctx
                .vfs_policy
                .get_writable_roots_with_cwd(&turn_ctx.cwd)
                .is_empty()
            {
                "read-only".to_string()
            } else {
                "workspace-write".to_string()
            }
        }
    }
}

fn apply_event_msg(metadata: &mut ProcessMetadata, event: &EventMsg) {
    match event {
        EventMsg::TokenCount(token_count) => {
            if let Some(info) = token_count.info.as_ref() {
                metadata.tokens_used = info.total_token_usage.total_tokens.max(0);
            }
        }
        EventMsg::UserMessage(user) => {
            if metadata.first_user_message.is_none() {
                metadata.first_user_message = user_message_preview(user);
            }
            if metadata.title.is_empty() {
                let title = strip_user_message_prefix(user.message.as_str());
                if !title.is_empty() {
                    metadata.title = title.to_string();
                }
            }
        }
        _ => {}
    }
}

fn apply_response_item(metadata: &mut ProcessMetadata, item: &ResponseItem) {
    let ResponseItem::Message { role, content, .. } = item else {
        return;
    };
    if role != "user" {
        return;
    }

    let preview = response_user_message_preview(content);
    if metadata.first_user_message.is_none() {
        metadata.first_user_message = preview.clone();
    }
    if metadata.title.is_empty()
        && let Some(preview) = preview
    {
        metadata.title = preview;
    }
}

fn strip_user_message_prefix(text: &str) -> &str {
    match text.find(USER_MESSAGE_BEGIN.as_str()) {
        Some(idx) => text[idx + USER_MESSAGE_BEGIN.len()..].trim(),
        None => text.trim(),
    }
}

fn response_user_message_preview(content: &[ContentItem]) -> Option<String> {
    if let Some(text) = content.iter().find_map(|item| match item {
        ContentItem::InputText { text } => Some(text.as_str()),
        ContentItem::InputImage { .. }
        | ContentItem::OutputText { .. }
        | ContentItem::Document { .. } => None,
    }) && let Some(preview) = cleanup_user_message_preview(text)
    {
        return Some(preview);
    }

    content
        .iter()
        .any(|item| matches!(item, ContentItem::InputImage { .. }))
        .then(|| IMAGE_ONLY_USER_MESSAGE_PLACEHOLDER.to_string())
}

fn cleanup_user_message_preview(mut text: &str) -> Option<String> {
    loop {
        let trimmed = text.trim_start();
        let Some(rest) = trimmed.strip_prefix(ENVIRONMENT_CONTEXT_OPEN_TAG) else {
            text = trimmed;
            break;
        };
        let close_idx = rest.find(ENVIRONMENT_CONTEXT_CLOSE_TAG)?;
        text = &rest[close_idx + ENVIRONMENT_CONTEXT_CLOSE_TAG.len()..];
    }

    let mut cleaned = strip_user_message_prefix(text).trim();
    if let Some(rest) = cleaned.strip_prefix("## My request for ")
        && let Some((_, request)) = rest.split_once(':')
    {
        cleaned = request.trim();
    }

    (!cleaned.is_empty()).then(|| cleaned.to_string())
}

fn user_message_preview(user: &UserMessageEvent) -> Option<String> {
    let message = strip_user_message_prefix(user.message.as_str());
    if !message.is_empty() {
        return Some(message.to_string());
    }
    if user
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
        || !user.local_images.is_empty()
    {
        return Some(IMAGE_ONLY_USER_MESSAGE_PLACEHOLDER.to_string());
    }
    None
}

pub(crate) fn enum_to_string<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests;
