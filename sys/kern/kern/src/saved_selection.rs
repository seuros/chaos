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
mod tests;
