use super::*;
use std::path::Path;

fn write_agent_file(dir: &Path, name: &str, contents: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().expect("agent file parent"))?;
    std::fs::write(&path, contents)?;
    Ok(path)
}

#[tokio::test]
async fn split_agent_role_declarations_are_rejected() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    std::fs::write(
        chaos_home.path().join(CONFIG_TOML_FILE),
        r#"[agents.researcher]
description = "Research role"
config_file = "./agents/researcher.toml"
"#,
    )?;
    let err = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .build()
        .await
        .expect_err("split declarations are no longer supported");
    assert!(err.to_string().contains("must be migrated"));
    let err = crate::user_settings::migrate(chaos_home.path(), true)
        .await
        .expect_err("migration must reject split declarations too");
    assert!(format!("{err:#}").contains("unknown field `researcher`"));
    Ok(())
}

#[tokio::test]
async fn agent_role_file_name_is_independent_of_filename() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let role_path = write_agent_file(
        &chaos_home.path().join("agents"),
        "researcher.toml",
        r#"
name = "archivist"
description = "Role metadata from file"
nickname_candidates = ["  Hypatia  ", "Noether"]
developer_instructions = "Research carefully"
model = "serpent"
"#,
    )?;
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .build()
        .await?;
    assert!(!config.agent_roles.contains_key("researcher"));
    let role = config
        .agent_roles
        .get("archivist")
        .expect("role should load");
    assert_eq!(role.description.as_deref(), Some("Role metadata from file"));
    assert_eq!(role.config_file.as_ref(), Some(&role_path));
    assert_eq!(
        role.nickname_candidates.as_deref(),
        Some(["Hypatia".to_string(), "Noether".to_string()].as_slice())
    );
    Ok(())
}

#[tokio::test]
async fn malformed_agent_files_are_dropped_with_warnings() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let agents_dir = chaos_home.path().join("agents");
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
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .fallback_cwd(Some(chaos_home.path().to_path_buf()))
        .build()
        .await?;
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
    let repo_root = TempDir::new()?;
    let nested_cwd = repo_root.path().join("packages").join("app");
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    std::fs::create_dir_all(&nested_cwd)?;
    crate::config::set_project_trust_level(
        chaos_home.path(),
        repo_root.path(),
        TrustLevel::Trusted,
    )
    .map_err(std::io::Error::other)?;

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
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(nested_cwd),
            ..Default::default()
        })
        .build()
        .await?;
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
async fn standalone_agent_role_sources_merge_with_precedence() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let repo_root = TempDir::new()?;
    std::fs::create_dir_all(repo_root.path().join(".git"))?;
    crate::config::set_project_trust_level(
        chaos_home.path(),
        repo_root.path(),
        TrustLevel::Trusted,
    )
    .map_err(std::io::Error::other)?;

    let home_agents = chaos_home.path().join("agents");
    write_agent_file(
        &home_agents,
        "researcher.toml",
        r#"
name = "researcher"
description = "Research role from home"
nickname_candidates = ["Noether"]
topics = ["research"]
catchphrases = ["Look closely."]
developer_instructions = "Research carefully"
model = "serpent"
"#,
    )?;
    let critic_path = write_agent_file(
        &home_agents,
        "critic.toml",
        "name = 'critic'\ndescription = 'Critic role'\ndeveloper_instructions = 'Critique carefully'",
    )?;
    let project_agents = repo_root.path().join(".chaos/agents");
    let researcher_path = write_agent_file(
        &project_agents,
        "researcher.toml",
        r#"
name = "researcher"
nickname_candidates = ["Hypatia"]
developer_instructions = "Research from project"
model = "fireship"
"#,
    )?;
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(repo_root.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;
    let role = config.agent_roles.get("researcher").expect("merged role");
    assert_eq!(role.description.as_deref(), Some("Research role from home"));
    assert_eq!(role.config_file.as_ref(), Some(&researcher_path));
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
    assert_eq!(
        config
            .agent_roles
            .get("critic")
            .and_then(|role| role.config_file.as_ref()),
        Some(&critic_path)
    );
    Ok(())
}

#[tokio::test]
async fn untrusted_project_agent_files_are_not_loaded() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
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
    let config = ConfigBuilder::default()
        .chaos_home(chaos_home.path().to_path_buf())
        .harness_overrides(ConfigOverrides {
            cwd: Some(repo_root.path().to_path_buf()),
            ..Default::default()
        })
        .build()
        .await?;
    assert!(!config.agent_roles.contains_key("researcher"));
    Ok(())
}
