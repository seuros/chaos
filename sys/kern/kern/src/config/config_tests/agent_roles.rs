use super::*;
use crate::config::AgentRoleSource;
use std::path::Path;

fn write_agent_file(dir: &Path, name: &str, contents: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().expect("agent file parent"))?;
    std::fs::write(&path, contents)?;
    Ok(path)
}

fn trusted_repo(chaos_home: &Path) -> std::io::Result<TempDir> {
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    crate::config::set_project_trust_level(chaos_home, repo_root.path(), TrustLevel::Trusted)
        .map_err(std::io::Error::other)?;
    Ok(repo_root)
}

async fn build(chaos_home: &TempDir, cwd: &Path) -> std::io::Result<Config> {
    ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(cwd.to_path_buf()),
            ..Default::default()
        })
        .build()
        .await
}

#[tokio::test]
async fn split_agent_role_declarations_are_rejected() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let err = crate::test_support::seed_user_settings_toml(
        chaos_home.path(),
        r#"[agents.researcher]
description = "Research role"
config_file = "./agents/researcher.toml"
"#,
    )
    .await
    .expect_err("split declarations are no longer supported");
    assert!(format!("{err:#}").contains("unknown field `researcher`"));
    Ok(())
}

#[tokio::test]
async fn agent_role_file_name_is_independent_of_filename() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path()).map_err(std::io::Error::other)?;
    let repo_root = trusted_repo(chaos_home.path())?;
    let role_path = write_agent_file(
        &repo_root.path().join(".chaos/agents"),
        "researcher.toml",
        r#"
name = "archivist"
description = "Role metadata from file"
nickname_candidates = ["  Hypatia  ", "Noether"]
developer_instructions = "Research carefully"
model = "serpent"
"#,
    )?;
    let config = build(&chaos_home, repo_root.path()).await?;
    assert!(!config.agent_roles.contains_key("researcher"));
    let role = config
        .agent_roles
        .get("archivist")
        .expect("role should load");
    assert_eq!(role.description.as_deref(), Some("Role metadata from file"));
    assert_eq!(role.source, Some(AgentRoleSource::File(role_path)));
    assert_eq!(
        role.nickname_candidates.as_deref(),
        Some(["Hypatia".to_string(), "Noether".to_string()].as_slice())
    );
    Ok(())
}

#[tokio::test]
async fn stored_roles_load_from_the_database() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let name = crate::test_support::seed_agent_role(
        chaos_home.path(),
        "md",
        "---\nname = 'archivist'\ndescription = 'Stored role'\n\
         nickname_candidates = ['Hypatia']\n---\nResearch carefully",
    )
    .await
    .map_err(std::io::Error::other)?;
    assert_eq!(name, "archivist");
    let config = build(&chaos_home, chaos_home.path()).await?;
    let role = config
        .agent_roles
        .get("archivist")
        .expect("stored role should load");
    assert_eq!(role.description.as_deref(), Some("Stored role"));
    let Some(AgentRoleSource::Database {
        config: layer,
        base_dir,
    }) = &role.source
    else {
        panic!(
            "stored role should carry a database source: {:?}",
            role.source
        );
    };
    assert_eq!(base_dir, &None);
    assert_eq!(
        layer
            .get("developer_instructions")
            .and_then(toml::Value::as_str),
        Some("Research carefully")
    );
    assert!(layer.get("description").is_none());
    Ok(())
}

#[tokio::test]
async fn disabled_and_mismatched_stored_roles_are_skipped() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::seed_agent_role(
        chaos_home.path(),
        "toml",
        "name = 'dormant'\ndescription = 'Disabled role'\ndeveloper_instructions = 'Rest'",
    )
    .await
    .map_err(std::io::Error::other)?;
    let runtime = crate::user_settings::open(chaos_home.path())
        .await
        .map_err(std::io::Error::other)?;
    let dormant = runtime
        .get_agent_role("dormant")
        .await
        .map_err(std::io::Error::other)?
        .expect("seeded role");
    runtime
        .set_agent_role_enabled("dormant", dormant.revision, false)
        .await
        .map_err(std::io::Error::other)?;
    runtime
        .put_agent_role(
            "mismatched",
            "toml",
            "name = 'other'\ndescription = 'Wrong name'\ndeveloper_instructions = 'Work'",
            None,
            0,
        )
        .await
        .map_err(std::io::Error::other)?;

    let config = build(&chaos_home, chaos_home.path()).await?;
    assert!(!config.agent_roles.contains_key("dormant"));
    assert!(!config.agent_roles.contains_key("mismatched"));
    assert!(!config.agent_roles.contains_key("other"));
    assert!(
        config.startup_warnings.iter().any(|warning| {
            warning.contains("stored agent role `mismatched`") && warning.contains("`other`")
        }),
        "expected a name-mismatch warning: {:?}",
        config.startup_warnings
    );
    Ok(())
}

#[tokio::test]
async fn malformed_agent_files_are_dropped_with_warnings() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path()).map_err(std::io::Error::other)?;
    let repo_root = trusted_repo(chaos_home.path())?;
    let agents_dir = repo_root.path().join(".chaos/agents");
    for (filename, contents) in [
        (
            "missing-instructions.toml",
            "name = 'missing-instructions'\ndescription = 'Missing instructions'\nmodel = 'serpent'",
        ),
        (
            "missing-name.toml",
            "description = 'Missing name'\ndeveloper_instructions = 'Research carefully'",
        ),
        (
            "missing-description.toml",
            "name = 'missing-description'\ndeveloper_instructions = 'Research carefully'",
        ),
        (
            "bad-nicknames.toml",
            "name = 'bad-nicknames'\ndescription = 'Bad nicknames'\ndeveloper_instructions = 'Research'\nnickname_candidates = ['Agent <One>']",
        ),
        (
            "reviewer.toml",
            "name = 'reviewer'\ndescription = 'Review role'\ndeveloper_instructions = 'Review carefully'",
        ),
    ] {
        write_agent_file(&agents_dir, filename, contents)?;
    }
    let config = build(&chaos_home, repo_root.path()).await?;
    assert_eq!(config.agent_roles.len(), 1);
    assert!(config.agent_roles.contains_key("reviewer"));
    for expected in [
        "must define `developer_instructions`",
        "must define a non-empty `name`",
        "must define a description",
        "may only contain ASCII letters",
    ] {
        assert!(
            config
                .startup_warnings
                .iter()
                .any(|warning| warning.contains(expected)),
            "missing warning {expected}: {:?}",
            config.startup_warnings
        );
    }
    Ok(())
}

#[tokio::test]
async fn discovers_multiple_standalone_agent_role_files() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path()).map_err(std::io::Error::other)?;
    let repo_root = trusted_repo(chaos_home.path())?;
    let nested_cwd = repo_root.path().join("packages").join("app");
    std::fs::create_dir_all(&nested_cwd)?;

    write_agent_file(
        &repo_root.path().join(".chaos/agents"),
        "root.toml",
        "name = 'researcher'\ndescription = 'from root'\ndeveloper_instructions = 'Research carefully'",
    )?;
    let nested_agents = repo_root.path().join("packages/.chaos/agents");
    write_agent_file(
        &nested_agents,
        "review/nested.toml",
        "name = 'reviewer'\ndescription = 'from nested'\nnickname_candidates = ['Atlas']\ndeveloper_instructions = 'Review carefully'",
    )?;
    write_agent_file(
        &nested_agents,
        "writer.md",
        "---\nname = 'writer'\ndescription = 'from sibling'\nnickname_candidates = ['Sagan']\n---\nWrite carefully",
    )?;
    let config = build(&chaos_home, &nested_cwd).await?;
    for (name, description, nickname) in [
        ("researcher", "from root", None),
        ("reviewer", "from nested", Some("Atlas")),
        ("writer", "from sibling", Some("Sagan")),
    ] {
        let role = config.agent_roles.get(name).expect("discovered role");
        assert_eq!(role.description.as_deref(), Some(description));
        assert_eq!(
            role.nickname_candidates
                .as_ref()
                .and_then(|names| names.first())
                .map(String::as_str),
            nickname
        );
    }
    Ok(())
}

#[tokio::test]
async fn stored_and_project_role_sources_merge_with_precedence() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::seed_agent_role(
        chaos_home.path(),
        "toml",
        r#"
name = "researcher"
description = "Research role from the database"
nickname_candidates = ["Noether"]
topics = ["research"]
catchphrases = ["Look closely."]
developer_instructions = "Research carefully"
model = "serpent"
"#,
    )
    .await
    .map_err(std::io::Error::other)?;
    crate::test_support::seed_agent_role(
        chaos_home.path(),
        "toml",
        "name = 'critic'\ndescription = 'Critic role'\ndeveloper_instructions = 'Critique carefully'",
    )
    .await
    .map_err(std::io::Error::other)?;
    let repo_root = trusted_repo(chaos_home.path())?;
    let researcher_path = write_agent_file(
        &repo_root.path().join(".chaos/agents"),
        "researcher.toml",
        r#"
name = "researcher"
nickname_candidates = ["Hypatia"]
developer_instructions = "Research from project"
model = "fireship"
"#,
    )?;
    let config = build(&chaos_home, repo_root.path()).await?;
    let role = config.agent_roles.get("researcher").expect("merged role");
    assert_eq!(
        role.description.as_deref(),
        Some("Research role from the database")
    );
    assert_eq!(role.source, Some(AgentRoleSource::File(researcher_path)));
    assert_eq!(
        role.nickname_candidates.as_deref(),
        Some(["Hypatia".to_string()].as_slice())
    );
    assert_eq!(
        role.topics.as_deref(),
        Some(["research".to_string()].as_slice())
    );
    assert_eq!(
        role.catchphrases.as_deref(),
        Some(["Look closely.".to_string()].as_slice())
    );
    assert!(matches!(
        config
            .agent_roles
            .get("critic")
            .and_then(|role| role.source.as_ref()),
        Some(AgentRoleSource::Database { .. })
    ));
    Ok(())
}

#[tokio::test]
async fn home_agents_folder_is_no_longer_read() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path()).map_err(std::io::Error::other)?;
    write_agent_file(
        &chaos_home.path().join("agents"),
        "researcher.toml",
        "name = 'researcher'\ndescription = 'Legacy home role'\n\
         developer_instructions = 'Research'",
    )?;
    let config = build(&chaos_home, chaos_home.path()).await?;
    assert!(!config.agent_roles.contains_key("researcher"));
    Ok(())
}

#[tokio::test]
async fn untrusted_project_agent_files_are_not_loaded() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    crate::test_support::bootstrap_sqlite(chaos_home.path()).map_err(std::io::Error::other)?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    crate::config::set_project_trust_level(
        chaos_home.path(),
        repo_root.path(),
        TrustLevel::Untrusted,
    )
    .map_err(std::io::Error::other)?;
    write_agent_file(
        &repo_root.path().join(".chaos/agents"),
        "researcher.toml",
        "name = 'researcher'\ndescription = 'Project role'\ndeveloper_instructions = 'Research'",
    )?;
    let config = build(&chaos_home, repo_root.path()).await?;
    assert!(!config.agent_roles.contains_key("researcher"));
    Ok(())
}
