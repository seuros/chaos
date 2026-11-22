use super::*;
use std::path::PathBuf;

#[test]
fn parses_claude_version_output() {
    assert_eq!(parse("2.1.330 (Claude Code)\n"), Some(Version(2, 1, 330)));
    assert_eq!(parse("2.1.327"), Some(Version(2, 1, 327)));
    assert_eq!(parse("Claude Code 2.1.330"), None);
    assert_eq!(parse("2.1"), None);
    assert_eq!(parse("2.1.330.4 (Claude Code)"), None);
    assert_eq!(parse(""), None);
}

#[test]
fn the_floor_is_2_1_330() {
    assert!(Version(2, 1, 329) < MIN_CLAUDE_CODE);
    assert!(Version(2, 1, 330) >= MIN_CLAUDE_CODE);
    assert!(Version(2, 2, 0) > MIN_CLAUDE_CODE);
    assert!(Version(3, 0, 0) > MIN_CLAUDE_CODE);
}

fn fake_cli(dir: &Path, version: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(format!("claude-{version}"));
    std::fs::write(
        &path,
        format!("#!/bin/sh\necho '{version} (Claude Code)'\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn an_old_claude_code_is_refused_by_name_and_a_current_one_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let old = fake_cli(dir.path(), "2.1.327");
    let error = ensure_supported(&old, None, &[])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("needs Claude Code 2.1.330 or later"),
        "{error}"
    );
    assert!(error.contains("found 2.1.327"), "{error}");
    let current = fake_cli(dir.path(), "2.1.330");
    assert_eq!(
        ensure_supported(&current, None, &[]).await.unwrap(),
        Version(2, 1, 330)
    );
}

#[tokio::test]
async fn the_probe_runs_where_the_child_will_run() {
    use std::os::unix::fs::PermissionsExt;
    let bin = tempfile::tempdir().unwrap();
    let new_project = tempfile::tempdir().unwrap();
    let old_project = tempfile::tempdir().unwrap();
    std::fs::write(old_project.path().join(".old-claude"), "").unwrap();
    let shim = bin.path().join("claude-shim");
    std::fs::write(
        &shim,
        "#!/bin/sh\nif [ -e .old-claude ]; then echo '2.1.327 (Claude Code)'; \
         else echo '2.1.330 (Claude Code)'; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        ensure_supported(&shim, Some(new_project.path()), &[])
            .await
            .is_ok()
    );
    assert!(
        ensure_supported(&shim, Some(old_project.path()), &[])
            .await
            .is_err()
    );
}
