use super::Representer;
use super::base_represent;
use crate::common::ResponsesApiRequest;
use chaos_abi::ReasoningEffort;
use chaos_abi::ResponseItem;

pub(crate) fn is_kimi_endpoint(base_url: &str) -> bool {
    let Ok(url) = url::Url::parse(base_url) else {
        return false;
    };
    matches!(
        (url.host_str(), url.path().trim_end_matches('/')),
        (Some("api.moonshot.ai" | "api.moonshot.cn"), "/v1")
            | (Some("api.kimi.ai" | "api.kimi.com"), "/coding/v1")
    )
}

/// Kimi replays plaintext reasoning summaries, not OpenAI encrypted state.
///
/// Wire contract: https://platform.kimi.ai/docs/api/responses
pub(super) struct KimiRepresenter;

impl Representer for KimiRepresenter {
    fn represent(&self, items: Vec<ResponseItem>) -> Vec<ResponseItem> {
        items
            .into_iter()
            .filter_map(base_represent)
            .filter(|item| {
                !matches!(
                    item,
                    ResponseItem::Reasoning {
                        encrypted_content: Some(_),
                        ..
                    } | ResponseItem::Compaction { .. }
                        | ResponseItem::CompactionTrigger {}
                )
            })
            .collect()
    }

    fn represent_reasoning_effort(&self, effort: ReasoningEffort) -> ReasoningEffort {
        // Kimi has no "none" effort; clamp disabled/minimal requests to its
        // lowest supported level. Keep this exhaustive as the ABI gains tiers.
        match effort {
            ReasoningEffort::None | ReasoningEffort::Minimal | ReasoningEffort::Low => {
                ReasoningEffort::Low
            }
            ReasoningEffort::Medium | ReasoningEffort::High => ReasoningEffort::High,
            ReasoningEffort::XHigh | ReasoningEffort::Max | ReasoningEffort::Ultra => {
                ReasoningEffort::Max
            }
        }
    }

    fn prepare_request(&self, request: &mut ResponsesApiRequest) {
        // Kimi always returns plaintext summaries; neither this include value
        // nor OpenAI's summary/verbosity/service-tier controls are supported.
        request
            .include
            .retain(|field| field != "reasoning.encrypted_content");
        if let Some(reasoning) = request.reasoning.as_mut() {
            reasoning.summary = None;
        }
        request.service_tier = None;
        if let Some(text) = request.text.as_mut() {
            text.verbosity = None;
            if text.format.is_none() {
                request.text = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::representer::SessionRepresenter;
    use chaos_ipc::models::ReasoningItemReasoningSummary;

    #[test]
    fn kimi_endpoint_matching_uses_exact_hosts_and_paths() {
        for base_url in [
            "https://api.moonshot.ai/v1",
            "https://api.moonshot.cn/v1/",
            "https://api.kimi.ai/coding/v1",
            "https://api.kimi.com/coding/v1/",
        ] {
            assert!(is_kimi_endpoint(base_url), "{base_url}");
        }
        for base_url in [
            "https://api.moonshot.ai.evil.test/v1",
            "https://api.moonshot.ai@evil.test/v1",
            "https://gateway.test/api.moonshot.ai/v1",
            "https://api.moonshot.ai/anthropic",
            "https://api.kimi.ai/v1",
            "not a URL",
        ] {
            assert!(!is_kimi_endpoint(base_url), "{base_url}");
        }
    }

    #[test]
    fn kimi_preserves_plaintext_reasoning_but_not_foreign_encrypted_state() {
        let reasoning = ResponseItem::Reasoning {
            id: "rs_kimi".into(),
            summary: vec![ReasoningItemReasoningSummary::SummaryText {
                text: "Check the repository before editing.".into(),
            }],
            content: None,
            encrypted_content: None,
        };
        let message = ResponseItem::Message {
            id: None,
            role: "system".into(),
            content: vec![],
            end_turn: None,
            phase: None,
        };
        let items = vec![
            message.clone(),
            reasoning.clone(),
            ResponseItem::Reasoning {
                id: "rs_openai".into(),
                summary: vec![],
                content: None,
                encrypted_content: Some("foreign-state".into()),
            },
            ResponseItem::CompactionTrigger {},
            ResponseItem::Compaction {
                encrypted_content: "foreign-compaction".into(),
            },
        ];
        let representer = SessionRepresenter::for_compatible_endpoint("https://api.moonshot.ai/v1");
        assert_eq!(representer.represent(items), vec![message, reasoning]);
    }
}
