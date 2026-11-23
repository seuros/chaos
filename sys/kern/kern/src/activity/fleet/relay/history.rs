//! Bounded, local context for an ordinary question, not a rollout export.
//! Never include reasoning, raw tool results or attachments. Operator-selected
//! sessions can contain sensitive text or diffs; pairing grants access to these.
use chaos_ipc::{ProcessId, protocol::EventMsg};
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    sync::{LazyLock, Mutex},
};

#[derive(Clone, Serialize)]
pub(crate) struct Entry {
    kind: &'static str,
    text: String,
}
static HISTORY: LazyLock<Mutex<HashMap<ProcessId, VecDeque<Entry>>>> =
    LazyLock::new(Default::default);

pub(crate) fn record(id: ProcessId, event: &EventMsg) {
    let extracted;
    let (kind, text) = match event {
        EventMsg::ItemCompleted(e) => {
            if std::env::var("CHAOS_MOBILE_OWNER").as_deref() != Ok("1") {
                return;
            }
            use chaos_ipc::items::{AgentMessageContent, TurnItem};
            use chaos_ipc::user_input::UserInput;
            let kind = match &e.item {
                TurnItem::UserMessage(m) => {
                    extracted = m
                        .content
                        .iter()
                        .filter_map(|i| {
                            if let UserInput::Text { text, .. } = i {
                                Some(text.as_str())
                            } else {
                                None
                            }
                        })
                        .flat_map(str::chars)
                        .take(4096)
                        .collect();
                    "user"
                }
                TurnItem::AgentMessage(m) => {
                    extracted = m
                        .content
                        .iter()
                        .map(|AgentMessageContent::Text { text }| text.as_str())
                        .flat_map(str::chars)
                        .take(4096)
                        .collect();
                    "assistant"
                }
                _ => return,
            };
            (kind, &extracted)
        }
        EventMsg::UserMessage(e) => ("user", &e.message),
        EventMsg::AgentMessage(e) => ("assistant", &e.message),
        EventMsg::TurnDiff(e) => ("diff", &e.unified_diff),
        EventMsg::ShutdownComplete => {
            HISTORY
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return;
        }
        _ => return,
    };
    if std::env::var("CHAOS_MOBILE_OWNER").as_deref() != Ok("1") || text.is_empty() {
        return;
    }
    let mut history = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    if history.len() >= 128 && !history.contains_key(&id) {
        return;
    }
    let recent = history.entry(id).or_default();
    if recent.len() == 16 {
        recent.pop_front();
    }
    recent.push_back(Entry {
        kind,
        text: text.chars().take(4096).collect(),
    });
}
pub(super) fn snapshot(id: ProcessId) -> VecDeque<Entry> {
    HISTORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&id)
        .cloned()
        .unwrap_or_default()
}
