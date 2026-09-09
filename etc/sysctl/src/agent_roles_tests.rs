use super::*;
use crate::types::AgentsToml;

fn parse(contents: &str) -> std::io::Result<ResolvedAgentRoleFile> {
    parse_agent_role_file_contents(
        contents,
        Path::new("researcher.toml"),
        Path::new("/tmp"),
        /*require_developer_instructions*/ true,
    )
}

#[test]
fn agent_limits_accept_only_operational_settings() {
    let limits: AgentsToml =
        toml::from_str("max_threads = 4\nmax_depth = 2\njob_max_runtime_seconds = 60")
            .expect("limits");
    assert_eq!(limits.max_threads, Some(4));
    assert_eq!(limits.max_depth, Some(2));
    assert_eq!(limits.job_max_runtime_seconds, Some(60));
    let err = toml::from_str::<AgentsToml>("[researcher]\ndescription = 'Research'")
        .expect_err("split roles must be rejected");
    assert!(err.to_string().contains("unknown field `researcher`"));
    let schema = serde_json::to_value(schemars::schema_for!(AgentsToml)).expect("schema");
    assert_eq!(schema["additionalProperties"], false);
}

#[test]
fn standalone_roles_require_name_and_instructions() {
    for (contents, expected) in [
        (
            "developer_instructions = 'Research'",
            "must define a non-empty `name`",
        ),
        (
            "name = 'researcher'",
            "must define `developer_instructions`",
        ),
        (
            "name = 'researcher'\ndeveloper_instructions = '  '",
            "cannot be blank",
        ),
    ] {
        let err = parse(contents).expect_err("incomplete role");
        assert!(err.to_string().contains(expected), "{err}");
    }
}

#[test]
fn builtin_default_can_omit_instructions_but_not_name() {
    let role = parse_agent_role_file_contents(
        "name = 'default'\ndescription = 'General purpose'",
        Path::new("default.toml"),
        Path::new("/tmp"),
        /*require_developer_instructions*/ false,
    )
    .expect("unconstrained builtin default");
    assert_eq!(role.role_name, "default");
    assert!(
        parse_agent_role_file_contents(
            "description = 'General purpose'",
            Path::new("default.toml"),
            Path::new("/tmp"),
            /*require_developer_instructions*/ false,
        )
        .is_err()
    );
}

#[test]
fn nickname_candidates_are_normalized() {
    let role = parse(
        "name = 'researcher'\ndeveloper_instructions = 'Research'\nnickname_candidates = ['  Hypatia  ', 'Noether']",
    )
    .expect("valid nicknames");
    assert_eq!(
        role.nickname_candidates,
        Some(vec!["Hypatia".into(), "Noether".into()])
    );
}

#[test]
fn invalid_nickname_candidates_are_rejected() {
    for (candidates, expected) in [
        ("[]", "must contain at least one name"),
        ("['Hypatia', ' Hypatia ']", "cannot contain duplicates"),
        ("['  ']", "cannot contain blank names"),
        ("['Agent <One>']", "may only contain ASCII letters"),
    ] {
        let err = parse(&format!(
            "name = 'researcher'\ndeveloper_instructions = 'Research'\nnickname_candidates = {candidates}",
        ))
        .expect_err("invalid nicknames");
        assert!(err.to_string().contains(expected), "{err}");
    }
}

#[test]
fn markdown_body_becomes_instructions_and_metadata_is_removed() {
    let role = parse_agent_role_file_contents(
        "---\nname = 'researcher'\ndescription = 'Research role'\ntopics = ['research']\n---\nResearch carefully.\n",
        Path::new("researcher.md"),
        Path::new("/tmp"),
        /*require_developer_instructions*/ true,
    )
    .expect("markdown role");
    assert_eq!(role.role_name, "researcher");
    assert_eq!(
        role.config["developer_instructions"].as_str(),
        Some("Research carefully.")
    );
    assert!(role.config.get("name").is_none());
    assert!(role.config.get("description").is_none());
    assert!(role.config.get("topics").is_none());
}

#[test]
fn duplicate_discovered_names_are_warned_and_first_file_wins() -> std::io::Result<()> {
    let dir = tempfile::tempdir()?;
    for (file, description) in [("a.toml", "First"), ("b.toml", "Second")] {
        std::fs::write(
            dir.path().join(file),
            format!(
                "name = 'researcher'\ndescription = '{description}'\ndeveloper_instructions = 'Research'"
            ),
        )?;
    }
    let mut warnings = Vec::new();
    let roles = discover_agent_roles_in_dir(dir.path(), &mut warnings)?;
    assert_eq!(roles.len(), 1);
    assert_eq!(roles["researcher"].description.as_deref(), Some("First"));
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("duplicate agent role name"))
    );
    Ok(())
}
