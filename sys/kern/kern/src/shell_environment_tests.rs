use super::*;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

fn test_shell(shell_type: ShellType, shell_path: &str) -> Shell {
    Shell {
        shell_type,
        shell_path: PathBuf::from(shell_path),
        shell_environment: crate::shell::empty_shell_environment_receiver(),
    }
}

#[test]
fn parse_environment_ignores_preamble_and_cwd_variables() -> Result<()> {
    let marker = b"\0chaos-shell-environment:test\0";
    let mut output = b"startup noise\n".to_vec();
    output.extend_from_slice(marker);
    output.extend_from_slice(b"PATH=/usr/bin\0PWD=/tmp\0OLDPWD=/\0NORMAL=value\0");

    let parsed = parse_environment_output(&output, marker)?;

    assert_eq!(
        parsed,
        HashMap::from([
            ("NORMAL".to_string(), "value".to_string()),
            ("PATH".to_string(), "/usr/bin".to_string()),
        ])
    );
    Ok(())
}

#[test]
fn parse_environment_preserves_multiline_values() -> Result<()> {
    let marker = b"\0chaos-shell-environment:test\0";
    let mut output = marker.to_vec();
    output.extend_from_slice(b"CERT=line-one\nline-two\0");

    let parsed = parse_environment_output(&output, marker)?;

    assert_eq!(
        parsed.get("CERT").map(String::as_str),
        Some("line-one\nline-two")
    );
    Ok(())
}

#[tokio::test]
async fn capture_does_not_create_snapshot_storage() -> Result<()> {
    let dir = tempdir()?;
    let shell = test_shell(ShellType::Bash, "/bin/bash");

    let environment = ShellEnvironment::try_new(dir.path(), &shell)
        .await
        .expect("environment should be captured");

    assert_eq!(environment.cwd, dir.path());
    assert!(environment.vars.contains_key("PATH"));
    assert!(!dir.path().join(LEGACY_SNAPSHOT_DIR).exists());
    Ok(())
}

#[tokio::test]
async fn actor_replaces_environment_when_cwd_changes() -> Result<()> {
    let dir = tempdir()?;
    let first_cwd = dir.path().join("first");
    let second_cwd = dir.path().join("second");
    fs::create_dir_all(&first_cwd).await?;
    fs::create_dir_all(&second_cwd).await?;
    let mut shell = test_shell(ShellType::Bash, "/bin/bash");
    let actor = ShellEnvironmentActor::spawn_inner(
        dir.path().to_path_buf(),
        ProcessId::new(),
        ShellEnvironmentStartup::Idle,
        &mut shell,
        None,
    );

    actor
        .refresh_and_wait(first_cwd.clone())
        .await
        .expect("first refresh");
    assert_eq!(
        shell.shell_environment().expect("first environment").cwd,
        first_cwd
    );

    actor
        .refresh_and_wait(second_cwd.clone())
        .await
        .expect("second refresh");
    assert_eq!(
        shell.shell_environment().expect("second environment").cwd,
        second_cwd
    );
    Ok(())
}

#[tokio::test]
async fn startup_removes_legacy_snapshot_storage_without_following_symlinks() -> Result<()> {
    let dir = tempdir()?;
    let target = dir.path().join("must-survive");
    fs::create_dir_all(&target).await?;
    fs::write(target.join("secret"), "keep").await?;
    let legacy = dir.path().join(LEGACY_SNAPSHOT_DIR);
    std::os::unix::fs::symlink(&target, &legacy)?;

    remove_legacy_snapshot_storage(dir.path()).await?;

    assert!(!legacy.exists());
    assert_eq!(fs::read_to_string(target.join("secret")).await?, "keep");
    Ok(())
}
