use super::apply_rollout_item;
use super::rollout_item_affects_process_metadata;
use crate::model::ProcessMetadata;
use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::permissions::SocketPolicy;
use chaos_ipc::permissions::VfsPolicy;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionMeta;
use chaos_ipc::protocol::SessionMetaLine;
use chaos_ipc::protocol::SessionSource;
use chaos_ipc::protocol::TurnContextItem;
use chaos_ipc::protocol::USER_MESSAGE_BEGIN;
use chaos_ipc::protocol::UserMessageEvent;

use pretty_assertions::assert_eq;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn response_item_user_messages_set_title_and_first_user_message() {
    let mut metadata = metadata_for_test();
    let item = RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "hello from response item".to_string(),
        }],
        end_turn: None,
        phase: None,
    });

    apply_rollout_item(&mut metadata, &item, "test-provider");

    assert_eq!(
        metadata.first_user_message.as_deref(),
        Some("hello from response item")
    );
    assert_eq!(metadata.title, "hello from response item");
    assert!(rollout_item_affects_process_metadata(&item));
}

#[test]
fn response_item_user_message_strips_environment_context_and_request_marker() {
    let mut metadata = metadata_for_test();
    let item = RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: concat!(
                "<environment_context>\n",
                "  <cwd>/tmp</cwd>\n",
                "</environment_context>\n\n",
                "## My request for FreeChaOS: Fix resume metadata"
            )
            .to_string(),
        }],
        end_turn: None,
        phase: None,
    });

    apply_rollout_item(&mut metadata, &item, "test-provider");

    assert_eq!(
        metadata.first_user_message.as_deref(),
        Some("Fix resume metadata")
    );
    assert_eq!(metadata.title, "Fix resume metadata");
}

#[test]
fn response_item_image_only_user_message_sets_image_placeholder_preview() {
    let mut metadata = metadata_for_test();
    let item = RolloutItem::ResponseItem(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputImage {
            image_url: "https://example.com/image.png".to_string(),
        }],
        end_turn: None,
        phase: None,
    });

    apply_rollout_item(&mut metadata, &item, "test-provider");

    assert_eq!(
        metadata.first_user_message.as_deref(),
        Some(super::IMAGE_ONLY_USER_MESSAGE_PLACEHOLDER)
    );
    assert_eq!(metadata.title, super::IMAGE_ONLY_USER_MESSAGE_PLACEHOLDER);
}

#[test]
fn event_msg_user_messages_set_title_and_first_user_message() {
    let mut metadata = metadata_for_test();
    let item = RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        message: format!("{} actual user request", *USER_MESSAGE_BEGIN),
        images: Some(vec![]),
        local_images: vec![],
        text_elements: vec![],
    }));

    apply_rollout_item(&mut metadata, &item, "test-provider");

    assert_eq!(
        metadata.first_user_message.as_deref(),
        Some("actual user request")
    );
    assert_eq!(metadata.title, "actual user request");
}

#[test]
fn event_msg_image_only_user_message_sets_image_placeholder_preview() {
    let mut metadata = metadata_for_test();
    let item = RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        message: String::new(),
        images: Some(vec!["https://example.com/image.png".to_string()]),
        local_images: vec![],
        text_elements: vec![],
    }));

    apply_rollout_item(&mut metadata, &item, "test-provider");

    assert_eq!(
        metadata.first_user_message.as_deref(),
        Some(super::IMAGE_ONLY_USER_MESSAGE_PLACEHOLDER)
    );
    assert_eq!(metadata.title, "");
}

#[test]
fn event_msg_blank_user_message_without_images_keeps_first_user_message_empty() {
    let mut metadata = metadata_for_test();
    let item = RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
        message: "   ".to_string(),
        images: Some(vec![]),
        local_images: vec![],
        text_elements: vec![],
    }));

    apply_rollout_item(&mut metadata, &item, "test-provider");

    assert_eq!(metadata.first_user_message, None);
    assert_eq!(metadata.title, "");
}

#[test]
fn turn_context_does_not_override_session_cwd() {
    let mut metadata = metadata_for_test();
    metadata.cwd = PathBuf::new();
    let process_id = metadata.id;

    apply_rollout_item(
        &mut metadata,
        &RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                id: process_id,
                forked_from_id: Some(
                    ProcessId::from_string(&Uuid::now_v7().to_string()).expect("thread id"),
                ),
                timestamp: "2026-02-26T00:00:00.000Z".to_string(),
                cwd: PathBuf::from("/child/worktree"),
                originator: "free_chaos".to_string(),
                cli_version: "0.0.0".to_string(),
                source: SessionSource::Cli,
                agent_nickname: None,
                agent_role: None,
                model_provider: Some("openai".to_string()),
                base_instructions: None,
                dynamic_tools: None,
                memory_mode: None,
            },
            git: None,
        }),
        "test-provider",
    );
    apply_rollout_item(
        &mut metadata,
        &RolloutItem::TurnContext(TurnContextItem {
            model_provider: "openai".into(),
            turn_id: Some("turn-1".to_string()),
            trace_id: None,
            cwd: PathBuf::from("/parent/workspace"),
            current_date: None,
            timezone: None,
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: VfsPolicy::unrestricted(),
            socket_policy: SocketPolicy::Enabled,
            network: None,
            model: "gpt-5".to_string(),
            personality: None,
            collaboration_mode: None,
            effort: None,
            summary: ReasoningSummary::Auto,
            user_instructions: None,
            developer_instructions: None,
            final_output_json_schema: None,
            truncation_policy: None,
        }),
        "test-provider",
    );

    assert_eq!(metadata.cwd, PathBuf::from("/child/worktree"));
    assert_eq!(metadata.sandbox_policy, "root-access");
    assert_eq!(metadata.approval_mode, "headless");
}

#[test]
fn turn_context_sets_cwd_when_session_cwd_missing() {
    let mut metadata = metadata_for_test();
    metadata.cwd = PathBuf::new();

    apply_rollout_item(
        &mut metadata,
        &RolloutItem::TurnContext(TurnContextItem {
            model_provider: "openai".into(),
            turn_id: Some("turn-1".to_string()),
            trace_id: None,
            cwd: PathBuf::from("/fallback/workspace"),
            current_date: None,
            timezone: None,
            approval_policy: ApprovalPolicy::Interactive,
            vfs_policy: VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
            socket_policy: SocketPolicy::Restricted,
            network: None,
            model: "gpt-5".to_string(),
            personality: None,
            collaboration_mode: None,
            effort: None,
            summary: ReasoningSummary::Auto,
            user_instructions: None,
            developer_instructions: None,
            final_output_json_schema: None,
            truncation_policy: None,
        }),
        "test-provider",
    );

    assert_eq!(metadata.cwd, PathBuf::from("/fallback/workspace"));
}

fn metadata_for_test() -> ProcessMetadata {
    let id = ProcessId::from_string(&Uuid::from_u128(42).to_string()).expect("thread id");
    let created_at = jiff::Timestamp::new(1_735_689_600, 0).expect("timestamp");
    ProcessMetadata {
        id,
        created_at,
        updated_at: created_at,
        source: "cli".to_string(),
        agent_nickname: None,
        agent_role: None,
        model_provider: "openai".to_string(),
        cwd: PathBuf::from("/tmp"),
        cli_version: "0.0.0".to_string(),
        title: String::new(),
        sandbox_policy: "read-only".to_string(),
        approval_mode: "interactive".to_string(),
        tokens_used: 1,
        first_user_message: None,
        archived_at: None,
        git_sha: None,
        git_branch: None,
        git_origin_url: None,
    }
}

#[test]
fn diff_fields_detects_changes() {
    let mut base = metadata_for_test();
    base.id = ProcessId::from_string(&Uuid::now_v7().to_string()).expect("thread id");
    base.title = "hello".to_string();
    let mut other = base.clone();
    other.tokens_used = 2;
    other.title = "world".to_string();
    let diffs = base.diff_fields(&other);
    assert_eq!(diffs, vec!["title", "tokens_used"]);
}
