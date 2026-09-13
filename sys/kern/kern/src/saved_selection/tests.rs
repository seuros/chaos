use super::*;
use chaos_ipc::config_types::{ModeKind, ReasoningSummary};
use chaos_ipc::protocol::{
    ApprovalPolicy, CompactedItem, ProcessRolledBackEvent, SessionMeta, SessionMetaLine,
    SocketPolicy, TurnStartedEvent, VfsPolicy,
};

fn turn(id: &str, provider: &str, model: &str) -> Vec<RolloutItem> {
    vec![
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_id: id.into(),
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Default,
        })),
        RolloutItem::TurnContext(TurnContextItem {
            turn_id: Some(id.into()),
            trace_id: None,
            cwd: "/tmp".into(),
            current_date: None,
            timezone: None,
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: VfsPolicy::unrestricted(),
            socket_policy: SocketPolicy::Restricted,
            network: None,
            model: model.into(),
            model_provider: provider.to_owned(),
            personality: None,
            collaboration_mode: None,
            effort: Some(ReasoningEffort::High),
            summary: ReasoningSummary::Auto,
            user_instructions: None,
            developer_instructions: None,
            final_output_json_schema: None,
            truncation_policy: None,
        }),
        RolloutItem::ResponseItem(ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        }),
    ]
}

#[test]
fn saved_selection_tracks_used_turns_rollback_compaction_and_fork_cutoff() {
    let mut items = turn("1", "first", "model-a");
    let first = saved_selection(&items).unwrap();
    items.extend(turn("2", "second", "model-b"));
    assert_eq!(saved_selection(&items).unwrap().provider, "second");
    assert_eq!(saved_selection(&items[..3]).unwrap(), first);
    // A fork cutoff may leave the next turn's context, but not its user message.
    assert_eq!(saved_selection(&items[..5]).unwrap(), first);
    items.push(RolloutItem::EventMsg(EventMsg::ProcessRolledBack(
        ProcessRolledBackEvent { num_turns: 1 },
    )));
    assert_eq!(saved_selection(&items).unwrap(), first);
    items.push(RolloutItem::Compacted(CompactedItem {
        message: "summary".into(),
        replacement_history: Some(vec![]),
    }));
    assert_eq!(saved_selection(&items).unwrap(), first);
    items.extend(turn("3", "third", "model-c"));
    assert_eq!(saved_selection(&items).unwrap().model, "model-c");
    // An unused selection/context must not supersede a real turn.
    items.extend(turn("4", "unused", "unused").into_iter().take(2));
    assert_eq!(saved_selection(&items).unwrap().provider, "third");
}

#[test]
fn saved_selection_uses_turn_provider_not_session_metadata() {
    assert!(saved_selection(&[]).is_none());
    let mut items = vec![RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            model_provider: Some("initial-provider".into()),
            ..Default::default()
        },
        git: None,
    })];
    assert!(saved_selection(&items).is_none());
    items.extend(turn("1", "current", "new-model"));
    assert_eq!(saved_selection(&items).unwrap().provider, "current");
    items.push(RolloutItem::EventMsg(EventMsg::ProcessRolledBack(
        ProcessRolledBackEvent { num_turns: 1 },
    )));
    assert!(saved_selection(&items).is_none());
}

#[test]
fn saved_selection_preserves_completed_background_turns() {
    let mut items = turn("1", "first", "first-model");
    // Completion turns may sample without introducing another user message.
    items.extend(turn("2", "second", "completion-model").into_iter().take(2));
    items.push(RolloutItem::EventMsg(EventMsg::TurnComplete(
        chaos_ipc::protocol::TurnCompleteEvent {
            turn_id: "2".into(),
            last_agent_message: None,
        },
    )));
    assert_eq!(saved_selection(&items).unwrap().model, "completion-model");
}

#[test]
fn saved_selection_restoration_is_atomic_and_honors_overrides() {
    let mut config = crate::config::test_config();
    let original_provider = config.model_provider_id.clone();
    let original_model = config.model.clone();
    let missing = turn("1", "not-configured", "model");
    assert!(restore_selection(&mut config, &missing, RestoreSelection::default()).is_err());
    assert_eq!(config.model_provider_id, original_provider);
    assert_eq!(config.model, original_model);
    for key in ["model", "model_provider"] {
        let intent = RestoreSelection::from_cli_overrides(&[(key.into(), "explicit".into())]);
        assert_eq!(
            restore_selection(&mut config, &missing, intent).unwrap(),
            None
        );
        assert_eq!(config.model, original_model);
    }
    config.model_reasoning_effort = Some(ReasoningEffort::Low);
    let items = turn("1", &original_provider, "saved-model");
    let intent =
        RestoreSelection::from_cli_overrides(&[("model_reasoning_effort".into(), "low".into())]);
    restore_selection(&mut config, &items, intent).unwrap();
    assert_eq!(config.model.as_deref(), Some("saved-model"));
    assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::Low));
    restore_selection(&mut config, &items, RestoreSelection::default()).unwrap();
    assert_eq!(config.model_reasoning_effort, Some(ReasoningEffort::High));
    let provider = config.model_provider.clone();
    config
        .model_providers
        .insert("other-account".into(), provider);
    let mut other = turn("2", "other-account", "other-model");
    if let RolloutItem::TurnContext(context) = &mut other[1] {
        context.effort = None;
    }
    restore_selection(&mut config, &other, RestoreSelection::default()).unwrap();
    assert_eq!(config.model_provider_id, "other-account");
    assert_eq!(config.model.as_deref(), Some("other-model"));
    assert_eq!(config.model_reasoning_effort, None);
    assert!(
        restore_selection(&mut config, &[], RestoreSelection::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(config.model.as_deref(), Some("other-model"));
}
