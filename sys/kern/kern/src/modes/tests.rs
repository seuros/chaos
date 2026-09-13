use super::*;

#[test]
fn loads_custom_modes_and_omits_instructions_from_resource() {
    let chaos_home = tempfile::tempdir().expect("temp chaos home");
    let modes_dir = chaos_home.path().join("modes");
    fs::create_dir_all(&modes_dir).expect("create modes dir");
    fs::write(
        modes_dir.join("research.md"),
        r#"+++
id = "research"
title = "Research"
description = "Evidence-first investigation."
reasoning_effort = "high"

[capabilities]
mutation = false
request_user_input = true
update_plan = true
+++
Secret full instructions that must not appear in the metadata resource.
"#,
    )
    .expect("write mode");

    let registry = ModeRegistry::load(chaos_home.path(), CollaborationModesConfig::default())
        .expect("load registry");
    let policy = ModePolicy::root(&registry);
    let resource = registry.resource_json(&policy).expect("serialize resource");

    assert!(registry.get("research").is_some());
    assert!(resource.contains("\"research\""));
    assert!(!resource.contains("Secret full instructions"));
}

#[test]
fn requested_child_mode_without_catalog_is_fixed() {
    let registry =
        ModeRegistry::builtins(CollaborationModesConfig::default()).expect("built-in registry");
    let parent = ModePolicy::root(&registry);
    let child = parent
        .child(
            &registry,
            ModeCapabilities::default(),
            Some(PLAN_MODE_ID),
            None,
            None,
        )
        .expect("child policy");

    assert_eq!(
        child,
        ModePolicy {
            active_mode: PLAN_MODE_ID.to_string(),
            allowed_modes: BTreeSet::from([PLAN_MODE_ID.to_string()]),
            switching_allowed: false,
        }
    );
}

#[test]
fn session_resource_only_exposes_policy_allowed_modes() {
    let registry =
        ModeRegistry::builtins(CollaborationModesConfig::default()).expect("built-in registry");
    let policy = ModePolicy {
        active_mode: PLAN_MODE_ID.to_string(),
        allowed_modes: BTreeSet::from([PLAN_MODE_ID.to_string()]),
        switching_allowed: false,
    };

    let resource = registry.resource_json(&policy).expect("serialize resource");
    assert!(
        !resource.contains('\n'),
        "model-facing JSON must be compact"
    );
    let resource: serde_json::Value = serde_json::from_str(&resource).expect("parse resource");

    assert_eq!(resource["active_mode"], PLAN_MODE_ID);
    assert_eq!(resource["switching_allowed"], false);
    assert_eq!(resource["modes"].as_array().expect("mode list").len(), 1);
    assert_eq!(resource["modes"][0]["id"], PLAN_MODE_ID);
}

#[test]
fn child_policy_cannot_broaden_parent_modes() {
    let registry =
        ModeRegistry::builtins(CollaborationModesConfig::default()).expect("built-in registry");
    let parent = ModePolicy {
        active_mode: PLAN_MODE_ID.to_string(),
        allowed_modes: BTreeSet::from([PLAN_MODE_ID.to_string()]),
        switching_allowed: false,
    };
    let error = parent
        .child(
            &registry,
            ModeCapabilities {
                mutation: false,
                request_user_input: true,
                update_plan: false,
            },
            Some(DEFAULT_MODE_ID),
            Some(&[DEFAULT_MODE_ID.to_string()]),
            Some(false),
        )
        .expect_err("must reject broadened child policy");

    assert!(error.contains("not allowed by the active parent mode"));
}
