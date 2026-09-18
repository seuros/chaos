use super::*;

#[test]
fn builtin_presets_returns_two_modes() {
    let presets = builtin_collaboration_mode_presets(CollaborationModesConfig::default());
    assert_eq!(presets.len(), 2);
}

#[test]
fn plan_preset_has_medium_reasoning() {
    let presets = builtin_collaboration_mode_presets(CollaborationModesConfig::default());
    let plan = presets
        .iter()
        .find(|p| p.mode == Some(ModeKind::Plan))
        .unwrap();
    assert_eq!(plan.reasoning_effort, Some(Some(ReasoningEffort::Medium)));
}

#[test]
fn default_mode_instructions_have_no_unfilled_placeholders() {
    let presets = builtin_collaboration_mode_presets(CollaborationModesConfig::default());
    let default = presets
        .iter()
        .find(|p| p.mode == Some(ModeKind::Default))
        .unwrap();
    let instructions = default
        .developer_instructions
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap();
    assert!(!instructions.contains("{{KNOWN_MODE_NAMES}}"));
    assert!(!instructions.contains("{{REQUEST_USER_INPUT_AVAILABILITY}}"));
    assert!(!instructions.contains("{{ASKING_QUESTIONS_GUIDANCE}}"));
}

#[test]
fn default_mode_instructions_mention_request_user_input_when_enabled() {
    let config = CollaborationModesConfig {
        default_mode_request_user_input: true,
    };
    let presets = builtin_collaboration_mode_presets(config);
    let default = presets
        .iter()
        .find(|p| p.mode == Some(ModeKind::Default))
        .unwrap();
    let instructions = default
        .developer_instructions
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap();
    assert!(instructions.contains("request_user_input"));
}
