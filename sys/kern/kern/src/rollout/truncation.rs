//! Thin wrapper around `chaos_rollout::truncation` that wires in the
//! core-specific user-message predicate (`event_mapping::parse_turn_item`).

use crate::event_mapping;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::RolloutItem;

/// Predicate: a `ResponseItem` is a user message when `parse_turn_item`
/// yields `Some(TurnItem::UserMessage(_))`.
fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { .. })
        && matches!(
            event_mapping::parse_turn_item(item),
            Some(TurnItem::UserMessage(_))
        )
}

pub(crate) fn truncate_rollout_before_nth_user_message_from_start(
    items: &[RolloutItem],
    n_from_start: usize,
) -> Vec<RolloutItem> {
    chaos_rollout::truncation::truncate_rollout_before_nth_user_message_from_start(
        items,
        n_from_start,
        is_user_message,
    )
}

#[cfg(test)]
#[path = "truncation_tests.rs"]
mod tests;
