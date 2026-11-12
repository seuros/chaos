use super::*;

#[test]
fn builtin_presets_only_expose_harness_when_enabled() {
    for harness_mode in [false, true] {
        let presets = builtin_collaboration_mode_presets(CollaborationModesConfig {
            harness_mode,
            ..Default::default()
        });
        let mut expected = vec![Some(ModeKind::Default), Some(ModeKind::Plan)];
        if harness_mode {
            expected.push(Some(ModeKind::Harness));
        }
        assert_eq!(
            presets.iter().map(|preset| preset.mode).collect::<Vec<_>>(),
            expected
        );
        let instructions = presets[0]
            .developer_instructions
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap();
        assert_eq!(instructions.contains("Harness"), harness_mode);
    }
}

#[test]
fn harness_availability_defaults_to_debug_build_or_debug_logging() {
    let config = CollaborationModesConfig::default();
    if cfg!(debug_assertions) {
        assert!(
            config.harness_mode,
            "debug builds must always expose Harness"
        );
    } else {
        assert_eq!(
            config.harness_mode,
            std::env::var_os("CHAOS_DEBUG_LOG_PATH").is_some()
        );
    }
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
fn builtin_mode_instructions_have_no_unfilled_placeholders() {
    let presets = builtin_collaboration_mode_presets(CollaborationModesConfig {
        harness_mode: true,
        ..Default::default()
    });
    for preset in presets {
        let instructions = preset.developer_instructions.unwrap().unwrap();
        assert!(!instructions.contains("{{"), "{}", preset.name);
        assert!(!instructions.contains("{%"), "{}", preset.name);
        if matches!(preset.mode, Some(ModeKind::Default | ModeKind::Harness)) {
            assert!(instructions.starts_with(&format!("# Collaboration Mode: {}", preset.name)));
        }
    }
}

#[test]
fn execution_modes_share_question_guidance_and_harness_adds_only_feedback() {
    for default_mode_request_user_input in [false, true] {
        let config = CollaborationModesConfig {
            default_mode_request_user_input,
            harness_mode: true,
        };
        let default = execution_mode_instructions(ModeKind::Default, config);
        let harness = execution_mode_instructions(ModeKind::Harness, config);
        assert_eq!(
            harness,
            format!(
                "{}{COLLABORATION_MODE_HARNESS}",
                default
                    .replace(
                        "# Collaboration Mode: Default",
                        "# Collaboration Mode: Harness"
                    )
                    .replace("in Default mode", "in Harness mode")
                    .replace("In Default mode", "In Harness mode")
            )
        );
        let availability = if default_mode_request_user_input {
            "available"
        } else {
            "unavailable"
        };
        assert!(harness.contains(&format!(
            "The `request_user_input` tool is {availability} in Harness mode."
        )));
        assert!(!default.contains("## Harness feedback"));
    }
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
