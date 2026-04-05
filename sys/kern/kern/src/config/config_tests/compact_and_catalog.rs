use super::*;

#[test]
fn cli_override_sets_compact_prompt() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let overrides = ConfigOverrides {
        compact_prompt: Some("Use the compact override".to_string()),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        ConfigToml::default(),
        overrides,
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.compact_prompt.as_deref(),
        Some("Use the compact override")
    );

    Ok(())
}

#[test]
fn loads_compact_prompt_from_file() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let workspace = chaos_home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;

    let prompt_path = workspace.join("compact_prompt.txt");
    std::fs::write(&prompt_path, "  summarize differently  ")?;

    let cfg = ConfigToml {
        experimental_compact_prompt_file: Some(AbsolutePathBuf::from_absolute_path(prompt_path)?),
        ..Default::default()
    };

    let overrides = ConfigOverrides {
        cwd: Some(workspace),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        overrides,
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(
        config.compact_prompt.as_deref(),
        Some("summarize differently")
    );

    Ok(())
}

#[test]
fn model_catalog_json_loads_from_path() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let catalog_path = chaos_home.path().join("catalog.json");
    let catalog = crate::test_support::test_models_response(&["skynet"]);
    std::fs::write(
        &catalog_path,
        serde_json::to_string(&catalog).expect("serialize catalog"),
    )?;

    let cfg = ConfigToml {
        model_catalog_json: Some(AbsolutePathBuf::from_absolute_path(catalog_path)?),
        ..Default::default()
    };

    let config = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )?;

    assert_eq!(config.model_catalog, Some(catalog));
    Ok(())
}

#[test]
fn model_catalog_json_rejects_empty_catalog() -> std::io::Result<()> {
    let chaos_home = TempDir::new()?;
    let catalog_path = chaos_home.path().join("catalog.json");
    std::fs::write(&catalog_path, r#"{"models":[]}"#)?;

    let cfg = ConfigToml {
        model_catalog_json: Some(AbsolutePathBuf::from_absolute_path(catalog_path)?),
        ..Default::default()
    };

    let err = Config::load_from_base_config_with_overrides(
        cfg,
        ConfigOverrides::default(),
        chaos_home.path().to_path_buf(),
    )
    .expect_err("empty custom catalog should fail config load");

    assert_eq!(err.kind(), ErrorKind::InvalidData);
    assert!(
        err.to_string().contains("must contain at least one model"),
        "unexpected error: {err}"
    );
    Ok(())
}
