use super::*;

#[test]
fn snapshot_does_not_follow_replaced_destination_or_leave_project() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let project = root.join("project");
    let directory = project.join("target");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&directory)?;
    std::fs::create_dir(&elsewhere)?;
    let snapshot = Snapshot {
        project: Some(project.clone()),
        candidates: vec![Candidate {
            tool: Tool::Cargo,
            directory: directory.clone(),
        }],
    };
    assert_eq!(snapshot.candidates_for(&project).count(), 1);
    assert_eq!(snapshot.candidates_for(&elsewhere).count(), 0);
    std::fs::remove_dir(&directory)?;
    std::os::unix::fs::symlink(&elsewhere, &directory)?;
    assert_eq!(snapshot.candidates_for(&project).count(), 0);
    Ok(())
}

#[test]
fn cargo_uses_repo_relative_paths_and_legacy_precedence() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    std::fs::create_dir(root.join(".cargo"))?;
    for dir in ["modern", "legacy"] {
        std::fs::create_dir(root.join(dir))?;
    }
    std::fs::write(
        root.join(".cargo/config.toml"),
        "[build]\ntarget-dir = 'modern'",
    )?;
    assert_eq!(cargo::destination(&root), Some(root.join("modern")));
    std::fs::write(root.join(".cargo/config"), "[build]\ntarget-dir = 'legacy'")?;
    assert_eq!(cargo::destination(&root), Some(root.join("legacy")));
    Ok(())
}

#[test]
fn mbx_external_path_is_only_a_candidate() -> std::io::Result<()> {
    let project = tempfile::tempdir()?;
    let cache = tempfile::tempdir()?;
    let root = project.path().canonicalize()?;
    let directory = cache.path().canonicalize()?;
    assert_eq!(mbx::destination(&root), None);
    let mut config = toml::Table::new();
    config.insert(
        "cache_dir".into(),
        toml::Value::String(directory.to_string_lossy().into_owned()),
    );
    std::fs::write(root.join(".mbx.toml"), config.to_string())?;
    assert_eq!(mbx::destination(&root), Some(directory));
    Ok(())
}
