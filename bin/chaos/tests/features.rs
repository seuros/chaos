use anyhow::Result;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

mod common;

use common::chaos_command;

#[tokio::test]
async fn features_enable_writes_feature_flag_to_config() -> Result<()> {
    let chaos_home = TempDir::new()?;

    let mut cmd = chaos_command(chaos_home.path())?;
    cmd.args(["features", "enable", "exec_permission_approvals"])
        .assert()
        .success()
        .stdout(contains(
            "Enabled feature `exec_permission_approvals` in config.toml.",
        ));

    let config = std::fs::read_to_string(chaos_home.path().join("config.toml"))?;
    assert!(config.contains("[features]"));
    assert!(config.contains("exec_permission_approvals = true"));

    Ok(())
}

#[tokio::test]
async fn features_disable_writes_feature_flag_to_config() -> Result<()> {
    let chaos_home = TempDir::new()?;

    // First enable so there is a config to mutate, then disable.
    let mut enable_cmd = chaos_command(chaos_home.path())?;
    enable_cmd
        .args(["features", "enable", "enable_fanout"])
        .assert()
        .success();

    let mut cmd = chaos_command(chaos_home.path())?;
    cmd.args(["features", "disable", "enable_fanout"])
        .assert()
        .success()
        .stdout(contains("Disabled feature `enable_fanout` in config.toml."));

    let config = std::fs::read_to_string(chaos_home.path().join("config.toml"))?;
    assert!(config.contains("[features]"));
    // Disabling a default-false feature removes the key rather than writing false.
    assert!(!config.contains("enable_fanout = true"));

    Ok(())
}

#[tokio::test]
async fn features_enable_unknown_feature_fails() -> Result<()> {
    let chaos_home = TempDir::new()?;

    let mut cmd = chaos_command(chaos_home.path())?;
    cmd.args(["features", "enable", "nonexistent_feature"])
        .assert()
        .failure()
        .stderr(contains("Unknown feature flag: nonexistent_feature"));

    Ok(())
}

#[tokio::test]
async fn features_list_is_sorted_alphabetically_by_feature_name() -> Result<()> {
    let chaos_home = TempDir::new()?;

    let mut cmd = chaos_command(chaos_home.path())?;
    let output = cmd
        .args(["features", "list"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output)?;

    let actual_names = stdout
        .lines()
        .map(|line| {
            line.split_once("  ")
                .map(|(name, _)| name.trim_end().to_string())
                .expect("feature list output should contain aligned columns")
        })
        .collect::<Vec<_>>();
    let mut expected_names = actual_names.clone();
    expected_names.sort();

    assert_eq!(actual_names, expected_names);

    Ok(())
}
