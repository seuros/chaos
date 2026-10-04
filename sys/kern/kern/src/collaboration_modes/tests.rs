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

#[test]
fn plan_instructions_are_rendered_for_the_mounted_backend_only() {
    use chaos_proc::planning::PlanningCapabilities;
    let postgres = plan_mode_instructions(PlanningCapabilities::POSTGRES);
    assert!(postgres.contains("separate acyclic dependency edges"));
    assert!(postgres.contains("Plans are shared across machines."));
    assert!(!postgres.contains("local database"));
    assert!(!postgres.contains("SQLite"));
    assert!(!postgres.contains("require PostgreSQL"));

    let sqlite = plan_mode_instructions(PlanningCapabilities::SQLITE);
    assert!(sqlite.contains("single-parent nesting and explicit sibling ordering"));
    assert!(sqlite.contains("local database"));
    assert!(!sqlite.contains("dependency edges"));
    assert!(!sqlite.contains("`link`"));
    assert!(!sqlite.contains("`graph`"));
    assert!(!sqlite.contains("PostgreSQL"));
    assert!(!sqlite.contains("shared across machines"));

    let unavailable = plan_mode_instructions(PlanningCapabilities::default());
    assert!(!unavailable.contains("## Durable planning tools"));
    assert!(!unavailable.contains("`plan` tool"));
    assert!(unavailable.contains("You must not perform **mutating** actions."));
    for rendered in [&postgres, &sqlite, &unavailable] {
        assert!(!rendered.contains("{%"));
        assert!(!rendered.contains("{{"));
        assert!(rendered.contains("<proposed_plan>"));
    }
}
