//! Session-local model selection restoration shared by interactive and exec clients.

use chaos_ipc::ProcessId;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::{EventMsg, RolloutItem, TurnContextItem};

use crate::{RolloutRecorder, config::Config};

/// Explicit choices are intent, not values inferred from configuration defaults.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreSelection {
    pub keep_current: bool,
    pub effort_override: bool,
}

impl RestoreSelection {
    pub fn from_cli_overrides(overrides: &[(String, toml::Value)]) -> Self {
        Self {
            keep_current: overrides
                .iter()
                .any(|(key, _)| matches!(key.as_str(), "model" | "model_provider")),
            effort_override: overrides
                .iter()
                .any(|(key, _)| key == "model_reasoning_effort"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SavedSelection {
    pub provider: String,
    pub model: String,
    pub effort: Option<ReasoningEffort>,
}

/// Read only the supplied history, so callers can pass an already-truncated fork.
pub fn saved_selection(items: &[RolloutItem]) -> Option<SavedSelection> {
    selection_from_items(items.iter())
}

fn selection_from_items<'a>(
    items: impl DoubleEndedIterator<Item = &'a RolloutItem>,
) -> Option<SavedSelection> {
    let context = last_used_context(items)?;
    Some(SavedSelection {
        provider: context.model_provider.clone(),
        model: context.model.clone(),
        effort: context.effort,
    })
}

/// Follow the same reverse user-turn boundaries as rollout reconstruction.
/// Compaction may clear a prompt baseline, but does not clear the last used model.
fn last_used_context<'a>(
    items: impl DoubleEndedIterator<Item = &'a RolloutItem>,
) -> Option<&'a TurnContextItem> {
    #[derive(Default)]
    struct Segment<'a> {
        turn_id: Option<&'a str>,
        user_turn: bool,
        completed_turn: bool,
        context: Option<&'a TurnContextItem>,
    }
    fn compatible(a: Option<&str>, b: Option<&str>) -> bool {
        a.is_none_or(|a| b.is_none_or(|b| a == b))
    }
    fn finish<'a>(segment: Segment<'a>, rollback: &mut usize) -> Option<&'a TurnContextItem> {
        if *rollback > 0 {
            if segment.user_turn {
                *rollback -= 1;
            }
            None
        } else if segment.user_turn || segment.completed_turn {
            segment.context
        } else {
            None
        }
    }
    let mut active: Option<Segment<'_>> = None;
    let mut rollback = 0usize;
    for item in items.rev() {
        match item {
            RolloutItem::EventMsg(EventMsg::ProcessRolledBack(event)) => {
                rollback =
                    rollback.saturating_add(usize::try_from(event.num_turns).unwrap_or(usize::MAX));
            }
            RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                let segment = active.get_or_insert_with(Segment::default);
                segment.turn_id.get_or_insert(event.turn_id.as_str());
                segment.completed_turn = true;
            }
            RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => {
                if let Some(id) = event.turn_id.as_deref() {
                    active
                        .get_or_insert_with(Segment::default)
                        .turn_id
                        .get_or_insert(id);
                }
            }
            RolloutItem::ResponseItem(ResponseItem::Message { role, .. }) if role == "user" => {
                active.get_or_insert_with(Segment::default).user_turn = true;
            }
            RolloutItem::TurnContext(context) => {
                let segment = active.get_or_insert_with(Segment::default);
                if segment.turn_id.is_none() {
                    segment.turn_id = context.turn_id.as_deref();
                }
                if compatible(segment.turn_id, context.turn_id.as_deref()) {
                    segment.context.get_or_insert(context);
                }
            }
            RolloutItem::EventMsg(EventMsg::TurnStarted(event)) => {
                if active
                    .as_ref()
                    .is_some_and(|s| compatible(s.turn_id, Some(&event.turn_id)))
                    && let Some(segment) = active.take()
                    && let Some(context) = finish(segment, &mut rollback)
                {
                    return Some(context);
                }
            }
            _ => {}
        }
    }
    active.and_then(|segment| finish(segment, &mut rollback))
}

/// Returns a user-facing warning when the retained history has no used turn.
/// Validation happens before any configuration is changed.
pub fn restore_selection(
    config: &mut Config,
    items: &[RolloutItem],
    intent: RestoreSelection,
) -> anyhow::Result<Option<String>> {
    if intent.keep_current {
        return Ok(None);
    }
    apply_selection(config, saved_selection(items), intent)
}

fn apply_selection(
    config: &mut Config,
    saved: Option<SavedSelection>,
    intent: RestoreSelection,
) -> anyhow::Result<Option<String>> {
    let Some(saved) = saved else {
        return Ok(Some(
            "Session has no retained used turn; keeping the current model/provider selection."
                .into(),
        ));
    };
    let provider = config.model_providers.get(&saved.provider).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "Saved provider `{}` is unavailable. Configure/authenticate that provider, \
             pass --provider or --model explicitly, or press Tab in the picker to keep the current selection.",
            saved.provider
        )
    })?;
    config.model = Some(saved.model);
    config.model_provider_id = saved.provider;
    config.model_provider = provider;
    if !intent.effort_override {
        config.model_reasoning_effort = saved.effort;
    }
    Ok(None)
}

pub async fn restore_process_selection(
    config: &mut Config,
    process_id: ProcessId,
    intent: RestoreSelection,
) -> anyhow::Result<Option<String>> {
    if intent.keep_current {
        return Ok(None);
    }
    apply_selection(config, load_saved_selection(process_id).await?, intent)
}

pub async fn load_saved_selection(
    process_id: ProcessId,
) -> std::io::Result<Option<SavedSelection>> {
    let journal = RolloutRecorder::get_journal_for_process(process_id).await?;
    Ok(selection_from_items(
        journal.items.iter().map(|entry| &entry.item),
    ))
}

#[cfg(test)]
mod tests {
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
        let intent = RestoreSelection::from_cli_overrides(&[(
            "model_reasoning_effort".into(),
            "low".into(),
        )]);
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
}
