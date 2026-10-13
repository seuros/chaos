use super::*;

#[test]
fn mode_switches_keep_storage_specific_instructions() {
    use chaos_proc::planning::PlanningCapabilities;
    let mut registry = ModeRegistry::builtins(CollaborationModesConfig::default()).unwrap();
    registry.configure_planning(PlanningCapabilities::POSTGRES);
    let base = CollaborationMode {
        mode: ModeKind::Default,
        settings: chaos_ipc::config_types::Settings {
            model: "test".into(),
            reasoning_effort: None,
            developer_instructions: None,
        },
    };
    let plan = registry.apply_mode(PLAN_MODE_ID, &base).unwrap();
    let text = plan.settings.developer_instructions.as_ref().unwrap();
    assert!(text.contains("shared across machines"));
    assert!(text.contains("append `clarify`"));
    assert!(text.contains("explicitly request `consolidate`"));
    assert!(!text.contains("local database"));
    let execution = registry.apply_mode(DEFAULT_MODE_ID, &plan).unwrap();
    assert!(
        !execution
            .settings
            .developer_instructions
            .as_ref()
            .unwrap()
            .contains("plan_progress")
    );
    assert_eq!(
        registry
            .apply_mode(PLAN_MODE_ID, &execution)
            .unwrap()
            .settings
            .developer_instructions,
        plan.settings.developer_instructions
    );
    registry.configure_planning(PlanningCapabilities::SQLITE);
    let resumed = registry.apply_mode(PLAN_MODE_ID, &execution).unwrap();
    let text = resumed.settings.developer_instructions.as_ref().unwrap();
    assert!(text.contains("local database"));
    assert!(!text.contains("shared across machines"));
    assert!(text.contains("append `clarify`"));
    registry.configure_planning(PlanningCapabilities::default());
    let unavailable = registry.apply_mode(PLAN_MODE_ID, &execution).unwrap();
    let text = unavailable
        .settings
        .developer_instructions
        .as_ref()
        .unwrap();
    assert!(!text.contains("append `clarify`"));
    assert!(!text.contains("`consolidate`"));
}

const RESEARCH_MODE: &str = r#"+++
id = "research"
title = "Research"
description = "Evidence-first investigation."
reasoning_effort = "high"

[capabilities]
mutation = false
request_user_input = true
planning_records = true
+++
Secret full instructions that must not appear in the metadata resource.
"#;

#[tokio::test]
async fn loads_custom_modes_and_omits_instructions_from_resource() -> anyhow::Result<()> {
    use crate::user_documents::DocumentKind;
    let chaos_home = tempfile::tempdir()?;
    crate::test_support::seed_document(
        chaos_home.path(),
        DocumentKind::Mode,
        "research",
        RESEARCH_MODE,
    )
    .await?;

    let registry =
        ModeRegistry::load(chaos_home.path(), CollaborationModesConfig::default()).await?;
    let policy = ModePolicy::root(&registry);
    let resource = registry.resource_json(&policy).expect("serialize resource");

    assert!(registry.get("research").is_some());
    assert!(resource.contains("\"research\""));
    assert!(!resource.contains("Secret full instructions"));
    Ok(())
}

#[test]
fn malformed_or_mismatched_stored_modes_are_skipped() -> anyhow::Result<()> {
    use crate::user_documents::{DocumentKind, StoredDocument};
    let document = |name: &str, source: &str| StoredDocument {
        kind: DocumentKind::Mode,
        name: name.to_string(),
        source: source.to_string(),
        revision: 1,
        enabled: true,
    };
    let unterminated = RESEARCH_MODE
        .replace("+++\nSecret", "Secret")
        .replace("id = \"research\"", "id = \"broken\"");
    let documents = [
        document("research", RESEARCH_MODE),
        document("broken", &unterminated),
        document("renamed", RESEARCH_MODE),
    ];

    let registry = ModeRegistry::from_documents(&documents, CollaborationModesConfig::default())?;

    assert!(registry.get("research").is_some());
    assert!(registry.get("broken").is_none());
    assert!(registry.get("renamed").is_none());

    let error = crate::user_documents::validate(DocumentKind::Mode, "renamed", RESEARCH_MODE)
        .expect_err("name must match the declared id");
    assert!(
        error.to_string().contains("declares id `research`"),
        "{error}"
    );
    Ok(())
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
                planning_records: true,
            },
            Some(DEFAULT_MODE_ID),
            Some(&[DEFAULT_MODE_ID.to_string()]),
            Some(false),
        )
        .expect_err("must reject broadened child policy");

    assert!(error.contains("not allowed by the active parent mode"));
}

#[test]
fn reserved_mode_ids_are_rejected_at_import() {
    for reserved in [DEFAULT_MODE_ID, PLAN_MODE_ID] {
        let source = RESEARCH_MODE.replace("id = \"research\"", &format!("id = \"{reserved}\""));
        let error = crate::user_documents::validate(
            crate::user_documents::DocumentKind::Mode,
            reserved,
            &source,
        )
        .expect_err("reserved id must not be stored")
        .to_string();
        assert!(error.contains("reserved"), "{error}");
    }
}
