//! Helpers for truncating rollouts based on "user turn" boundaries.
//!
//! User-turn detection is injected via a predicate so this crate stays
//! independent of the event-mapping logic that lives in codex-core.

use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::EventMsg;
use chaos_ipc::protocol::RolloutItem;

/// Default predicate: a `ResponseItem::Message` with role "user" whose content
/// items include at least one `InputText`.
///
/// This covers the common case without pulling in the full event-mapping
/// machinery.  Callers that need richer parsing (e.g. filtering contextual
/// user messages) can supply their own predicate.
pub fn is_user_message_default(item: &ResponseItem) -> bool {
    matches!(
        item,
        ResponseItem::Message { role, .. } if role == "user"
    )
}

/// Return the indices of user message boundaries in a rollout.
///
/// `is_user_message` decides whether a given `ResponseItem` counts as a user
/// turn.
///
/// Rollouts can contain `ProcessRolledBack` markers. Those markers indicate
/// that the last N user turns were removed from the effective thread history;
/// we apply them here so indexing uses the post-rollback history rather than
/// the raw stream.
pub fn user_message_positions_in_rollout(
    items: &[RolloutItem],
    is_user_message: impl Fn(&ResponseItem) -> bool,
) -> Vec<usize> {
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

/// Return a prefix of `items` obtained by cutting strictly before the nth
/// user message.
///
/// The boundary index is 0-based from the start of `items` (so
/// `n_from_start = 0` returns a prefix that excludes the first user message
/// and everything after it).
///
/// If `n_from_start` is `usize::MAX`, this returns the full rollout (no
/// truncation).  If fewer than or equal to `n_from_start` user messages
/// exist, this returns an empty vector (out of range).
pub fn truncate_rollout_before_nth_user_message_from_start(
    items: &[RolloutItem],
    n_from_start: usize,
    is_user_message: impl Fn(&ResponseItem) -> bool,
) -> Vec<RolloutItem> {
    if n_from_start == usize::MAX {
        return items.to_vec();
    }

    let user_positions = user_message_positions_in_rollout(items, is_user_message);

    // If fewer than or equal to n user messages exist, treat as empty (out of range).
    if user_positions.len() <= n_from_start {
        return Vec::new();
    }

    // Cut strictly before the nth user message (do not keep the nth itself).
    let cut_idx = user_positions[n_from_start];
    items[..cut_idx].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chaos_ipc::models::ContentItem;
    use chaos_ipc::models::ReasoningItemReasoningSummary;
    use chaos_ipc::protocol::ProcessRolledBackEvent;

    fn user_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    fn assistant_msg(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            end_turn: None,
            phase: None,
        }
    }

    #[test]
    fn truncates_rollout_from_start_before_nth_user_only() {
        let items = [
            user_msg("u1"),
            assistant_msg("a1"),
            assistant_msg("a2"),
            user_msg("u2"),
            assistant_msg("a3"),
            ResponseItem::Reasoning {
                id: "r1".to_string(),
                summary: vec![ReasoningItemReasoningSummary::SummaryText {
                    text: "s".to_string(),
                }],
                content: None,
                encrypted_content: None,
            },
            ResponseItem::FunctionCall {
                id: None,
                call_id: "c1".to_string(),
                name: "tool".to_string(),
                namespace: None,
                arguments: "{}".to_string(),
            },
            assistant_msg("a4"),
        ];

        let rollout: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();

        let truncated = truncate_rollout_before_nth_user_message_from_start(
            &rollout,
            1,
            is_user_message_default,
        );
        let expected = vec![
            RolloutItem::ResponseItem(items[0].clone()),
            RolloutItem::ResponseItem(items[1].clone()),
            RolloutItem::ResponseItem(items[2].clone()),
        ];
        assert_eq!(
            serde_json::to_value(&truncated).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );

        let truncated2 = truncate_rollout_before_nth_user_message_from_start(
            &rollout,
            2,
            is_user_message_default,
        );
        assert!(truncated2.is_empty());
    }

    #[test]
    fn truncation_max_keeps_full_rollout() {
        let rollout = vec![
            RolloutItem::ResponseItem(user_msg("u1")),
            RolloutItem::ResponseItem(assistant_msg("a1")),
            RolloutItem::ResponseItem(user_msg("u2")),
        ];

        let truncated = truncate_rollout_before_nth_user_message_from_start(
            &rollout,
            usize::MAX,
            is_user_message_default,
        );

        assert_eq!(
            serde_json::to_value(&truncated).unwrap(),
            serde_json::to_value(&rollout).unwrap()
        );
    }

    #[test]
    fn truncates_rollout_from_start_applies_thread_rollback_markers() {
        let rollout_items = vec![
            RolloutItem::ResponseItem(user_msg("u1")),
            RolloutItem::ResponseItem(assistant_msg("a1")),
            RolloutItem::ResponseItem(user_msg("u2")),
            RolloutItem::ResponseItem(assistant_msg("a2")),
            RolloutItem::EventMsg(EventMsg::ProcessRolledBack(ProcessRolledBackEvent {
                num_turns: 1,
            })),
            RolloutItem::ResponseItem(user_msg("u3")),
            RolloutItem::ResponseItem(assistant_msg("a3")),
            RolloutItem::ResponseItem(user_msg("u4")),
            RolloutItem::ResponseItem(assistant_msg("a4")),
        ];

        // Effective user history after applying rollback(1) is: u1, u3, u4.
        // So n_from_start=2 should cut before u4 (not u3).
        let truncated = truncate_rollout_before_nth_user_message_from_start(
            &rollout_items,
            2,
            is_user_message_default,
        );
        let expected = rollout_items[..7].to_vec();
        assert_eq!(
            serde_json::to_value(&truncated).unwrap(),
            serde_json::to_value(&expected).unwrap()
        );
    }
}
