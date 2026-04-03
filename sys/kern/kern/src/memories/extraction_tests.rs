use super::JobOutcome;
use super::JobResult;
use super::aggregate_stats;
use super::job::serialize_filtered_rollout_response_items;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::openai_models::ReasoningEffortPreset;
use chaos_ipc::protocol::RolloutItem;
use chaos_ipc::protocol::TokenUsage;
use pretty_assertions::assert_eq;

#[test]
fn serializes_memory_rollout_with_agents_removed_but_environment_kept() {
    let mixed_contextual_message = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "# AGENTS.md instructions for /tmp\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>"
                    .to_string(),
            },
            ContentItem::InputText {
                text: "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>".to_string(),
            },
        ],
        end_turn: None,
        phase: None,
    };
    let skill_message = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>"
                .to_string(),
        }],
        end_turn: None,
        phase: None,
    };
    let subagent_message = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "<subagent_notification>{\"agent_id\":\"a\",\"status\":\"completed\"}</subagent_notification>"
                .to_string(),
        }],
        end_turn: None,
        phase: None,
    };

    let serialized = serialize_filtered_rollout_response_items(&[
        RolloutItem::ResponseItem(mixed_contextual_message),
        RolloutItem::ResponseItem(skill_message),
        RolloutItem::ResponseItem(subagent_message.clone()),
    ])
    .expect("serialize");
    let parsed: Vec<ResponseItem> = serde_json::from_str(&serialized).expect("parse");

    assert_eq!(
        parsed,
        vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>"
                        .to_string(),
                }],
                end_turn: None,
                phase: None,
            },
            subagent_message,
        ]
    );
}

#[test]
fn count_outcomes_sums_token_usage_across_all_jobs() {
    let counts = aggregate_stats(vec![
        JobResult {
            outcome: JobOutcome::SucceededWithOutput,
            token_usage: Some(TokenUsage {
                input_tokens: 10,
                cached_input_tokens: 2,
                output_tokens: 3,
                reasoning_output_tokens: 1,
                total_tokens: 13,
            }),
        },
        JobResult {
            outcome: JobOutcome::SucceededNoOutput,
            token_usage: Some(TokenUsage {
                input_tokens: 7,
                cached_input_tokens: 1,
                output_tokens: 2,
                reasoning_output_tokens: 0,
                total_tokens: 9,
            }),
        },
        JobResult {
            outcome: JobOutcome::Failed,
            token_usage: None,
        },
    ]);

    assert_eq!(counts.claimed, 3);
    assert_eq!(counts.succeeded_with_output, 1);
    assert_eq!(counts.succeeded_no_output, 1);
    assert_eq!(counts.failed, 1);
    assert_eq!(
        counts.total_token_usage,
        Some(TokenUsage {
            input_tokens: 17,
            cached_input_tokens: 3,
            output_tokens: 5,
            reasoning_output_tokens: 1,
            total_tokens: 22,
        })
    );
}

#[test]
fn count_outcomes_keeps_usage_empty_when_no_job_reports_it() {
    let counts = aggregate_stats(vec![
        JobResult {
            outcome: JobOutcome::SucceededWithOutput,
            token_usage: None,
        },
        JobResult {
            outcome: JobOutcome::Failed,
            token_usage: None,
        },
    ]);

    assert_eq!(counts.claimed, 2);
    assert_eq!(counts.total_token_usage, None);
}

#[test]
fn reasoning_effort_is_none_when_model_has_no_supported_levels() {
    use crate::memories::reasoning_effort_for_model;
    use crate::models_manager::model_info::model_info_from_slug;

    // model_info_from_slug returns fallback metadata with empty supported_reasoning_levels.
    let model_info = model_info_from_slug("some-unknown-model");
    assert!(model_info.supported_reasoning_levels.is_empty());
    let effort = reasoning_effort_for_model(&model_info, ReasoningEffort::Low);
    assert_eq!(effort, None);
}

#[test]
fn reasoning_effort_is_some_when_model_supports_reasoning() {
    use crate::memories::reasoning_effort_for_model;
    use crate::models_manager::model_info::model_info_from_slug;

    let mut model_info = model_info_from_slug("reasoning-model");
    model_info.supported_reasoning_levels = vec![ReasoningEffortPreset {
        effort: ReasoningEffort::Low,
        description: "low".to_string(),
    }];
    let effort = reasoning_effort_for_model(&model_info, ReasoningEffort::Low);
    assert_eq!(effort, Some(ReasoningEffort::Low));
}

#[test]
fn reasoning_effort_preserves_requested_default_effort() {
    use crate::memories::reasoning_effort_for_model;
    use crate::models_manager::model_info::model_info_from_slug;

    let mut model_info = model_info_from_slug("reasoning-model");
    model_info.supported_reasoning_levels = vec![
        ReasoningEffortPreset {
            effort: ReasoningEffort::Low,
            description: "low".to_string(),
        },
        ReasoningEffortPreset {
            effort: ReasoningEffort::Medium,
            description: "medium".to_string(),
        },
    ];
    assert_eq!(
        reasoning_effort_for_model(&model_info, ReasoningEffort::Medium),
        Some(ReasoningEffort::Medium),
    );
}
