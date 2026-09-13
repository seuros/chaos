use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ApprovalsReviewer;
use chaos_ipc::protocol::AgentMessageContentDeltaEvent;
use chaos_ipc::protocol::AgentMessageEvent;
use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::BackgroundEventEvent;
use chaos_ipc::protocol::ErrorEvent;
use chaos_ipc::protocol::ExecCommandEndEvent;
use chaos_ipc::protocol::ExecCommandSource;
use chaos_ipc::protocol::ExecCommandStatus;
use chaos_ipc::protocol::SandboxPolicy;
use chaos_ipc::protocol::SessionConfiguredEvent;
use chaos_ipc::protocol::TokenCountEvent;
use chaos_ipc::protocol::TokenUsage;
use chaos_ipc::protocol::TokenUsageInfo;
use chaos_ipc::protocol::TurnCompleteEvent;
use pretty_assertions::assert_eq;

use super::*;

fn session_configured() -> SessionConfiguredEvent {
    SessionConfiguredEvent {
        session_id: ProcessId::default(),
        forked_from_id: None,
        process_name: None,
        model: String::new(),
        model_provider_id: "chassis-test".to_string(),
        service_tier: None,
        approval_policy: ApprovalPolicy::default(),
        approvals_reviewer: ApprovalsReviewer::default(),
        vfs_policy: chaos_ipc::protocol::VfsPolicy::from(&SandboxPolicy::new_read_only_policy()),
        socket_policy: chaos_ipc::protocol::SocketPolicy::from(
            &SandboxPolicy::new_read_only_policy(),
        ),
        cwd: PathBuf::from("/"),
        reasoning_effort: None,
        history_log_id: 0,
        history_entry_count: 0,
        initial_messages: None,
        network_proxy: None,
    }
}

#[test]
fn full_turn_lifecycle_reduces_to_transcript_state() {
    let mut state = FrontendState::new();
    state.apply_event_msg(EventMsg::SessionConfigured(session_configured()));
    assert_eq!(SessionStatus::Ready, state.status);

    state.record_user_submission("hello".to_string());
    assert_eq!(TurnStatus::InFlight, state.turn);
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptEntry::User { text }) if text == "hello"
    ));

    state.apply_event_msg(EventMsg::AgentMessageContentDelta(
        AgentMessageContentDeltaEvent {
            process_id: "p".to_string(),
            turn_id: "t".to_string(),
            item_id: "item-1".to_string(),
            delta: "hi".to_string(),
        },
    ));
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptEntry::Agent { content }) if content == "hi"
    ));

    state.apply_event_msg(EventMsg::AgentMessage(AgentMessageEvent {
        message: "hi there".to_string(),
        phase: None,
    }));
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptEntry::Agent { content }) if content == "hi there"
    ));

    state.apply_event_msg(EventMsg::TurnComplete(TurnCompleteEvent {
        turn_id: "t1".to_string(),
        last_agent_message: Some("hi there".to_string()),
    }));
    assert_eq!(TurnStatus::Idle, state.turn);
    assert_eq!(0, state.pending_stream_count());
    assert_eq!(0, state.pending_call_count());
}

#[test]
fn errors_and_token_usage_update_state() {
    let mut state = FrontendState::new();
    state.record_user_submission("hello".to_string());

    state.apply_event_msg(EventMsg::TokenCount(TokenCountEvent {
        info: Some(TokenUsageInfo {
            total_token_usage: TokenUsage {
                input_tokens: 1,
                output_tokens: 2,
                total_tokens: 3,
                ..Default::default()
            },
            last_token_usage: TokenUsage::default(),
            model_context_window: None,
        }),
        rate_limits: None,
        provider_request_started: false,
    }));
    assert_eq!(
        Some(3),
        state.token_usage.as_ref().map(|usage| usage.total_tokens)
    );

    state.apply_event_msg(EventMsg::BackgroundEvent(BackgroundEventEvent {
        message: "queued".to_string(),
    }));
    state.apply_event_msg(EventMsg::Error(ErrorEvent {
        message: "boom".to_string(),
        chaos_error_info: Some(ChaosErrorInfo::ContextWindowExceeded),
    }));

    assert_eq!(TurnStatus::Idle, state.turn);
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptEntry::Notice {
            level: NoticeLevel::Error,
            text,
        }) if text.contains("context window exceeded") && text.contains("boom")
    ));
}

#[test]
fn orphan_exec_end_still_renders() {
    let mut state = FrontendState::new();
    state.apply_event_msg(EventMsg::ExecCommandEnd(ExecCommandEndEvent {
        call_id: "call-1".to_string(),
        command: vec!["echo".to_string(), "hi".to_string()],
        aggregated_output: "hi".to_string(),
        parsed_cmd: Vec::new(),
        cwd: PathBuf::from("/tmp"),
        exit_code: 0,
        stdout: "hi".to_string(),
        stderr: String::new(),
        duration: std::time::Duration::from_secs(1),
        formatted_output: String::new(),
        status: ExecCommandStatus::Completed,
        source: ExecCommandSource::Agent,
        process_id: None,
        turn_id: "t".to_string(),
        interaction_input: None,
    }));

    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptEntry::Exec {
            exit_code: Some(0),
            output,
            ..
        }) if output == "hi"
    ));
}
