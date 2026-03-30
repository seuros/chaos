use crate::event_mapping;
use chaos_ipc::items::TurnItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::RolloutItem;

fn is_user_message(item: &ResponseItem) -> bool {
    matches!(item, ResponseItem::Message { .. })
        && matches!(
            event_mapping::parse_turn_item(item),
            Some(TurnItem::UserMessage(_))
        )
}

fn user_message_positions_in_history(items: &[RolloutItem]) -> Vec<usize> {
    let mut user_positions = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        match item {
            RolloutItem::ResponseItem(resp) if is_user_message(resp) => {
                user_positions.push(idx);
            }
            RolloutItem::EventMsg(EventMsg::ProcessRolledBack(rollback)) => {
                let num_turns = usize::try_from(rollback.num_turns).unwrap_or(usize::MAX);
                let new_len = user_positions.len().saturating_sub(num_turns);
                user_positions.truncate(new_len);
            }
            _ => {}
        }
    }
    user_positions
}

pub(crate) fn truncate_rollout_before_nth_user_message_from_start(
    items: &[RolloutItem],
    n_from_start: usize,
) -> Vec<RolloutItem> {
    if n_from_start == usize::MAX {
        return items.to_vec();
    }

    let user_positions = user_message_positions_in_history(items);
    if user_positions.len() <= n_from_start {
        return Vec::new();
    }

    let cut_idx = user_positions[n_from_start];
    items[..cut_idx].to_vec()
}

#[cfg(test)]
#[path = "truncation_tests.rs"]
mod tests;
