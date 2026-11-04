use super::*;

#[test]
fn fuzzy_search_returns_matching_file_paths() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("src/nested")).expect("create dirs");
    std::fs::write(dir.join("src/nested/alpha_widget.rs"), "").expect("write alpha");
    std::fs::write(dir.join("src/beta.rs"), "").expect("write beta");

    let matches = run_locate_search(
        "awrs",
        dir.to_path_buf(),
        NonZero::new(10).expect("non-zero limit"),
        true,
    )
    .expect("locate search");

    assert!(
        matches
            .matches
            .iter()
            .any(|path| path.ends_with("alpha_widget.rs")),
        "matches: {matches:?}"
    );
}

#[test]
fn glob_search_returns_matching_file_paths() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("sys/kern/templates")).expect("create dirs");
    std::fs::write(dir.join("README.md"), "").expect("write readme");
    std::fs::write(dir.join("sys/kern/templates/plan.md"), "").expect("write plan");
    std::fs::write(dir.join("sys/kern/templates/plan.rs"), "").expect("write rust");

    let matches = run_locate_search(
        "*.md",
        dir.to_path_buf(),
        NonZero::new(10).expect("non-zero limit"),
        true,
    )
    .expect("locate search");

    assert_eq!(matches.total_match_count, 2, "matches: {matches:?}");
    assert!(
        matches
            .matches
            .iter()
            .any(|path| path.ends_with("README.md")),
        "matches: {matches:?}"
    );
    assert!(
        matches
            .matches
            .iter()
            .any(|path| path.ends_with("sys/kern/templates/plan.md")),
        "matches: {matches:?}"
    );
    assert!(
        !matches
            .matches
            .iter()
            .any(|path| path.ends_with("sys/kern/templates/plan.rs")),
        "matches: {matches:?}"
    );
}

#[test]
fn path_glob_search_matches_relative_paths() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::create_dir_all(dir.join("sys/kern/templates")).expect("create dirs");
    std::fs::create_dir_all(dir.join("docs")).expect("create docs");
    std::fs::write(dir.join("sys/kern/templates/plan.md"), "").expect("write plan");
    std::fs::write(dir.join("docs/plan.md"), "").expect("write docs");

    let matches = run_locate_search(
        "sys/*/templates/*.md",
        dir.to_path_buf(),
        NonZero::new(10).expect("non-zero limit"),
        true,
    )
    .expect("locate search");

    assert_eq!(matches.total_match_count, 1, "matches: {matches:?}");
    assert!(
        matches
            .matches
            .iter()
            .any(|path| path.ends_with("sys/kern/templates/plan.md")),
        "matches: {matches:?}"
    );
}

#[tokio::test]
async fn structured_results_preserve_paths_containing_newlines() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let odd_name = "line\nbreak.md";
    std::fs::write(temp.path().join(odd_name), "content").expect("write file");

    let result = execute_structured(&serde_json::json!({
        "pattern": "line",
        "path": temp.path().to_string_lossy(),
        "include_hidden": true
    }))
    .await
    .expect("locate files");
    let expected = temp.path().join(odd_name).to_string_lossy().into_owned();
    assert_eq!(result["matches"], serde_json::json!([expected]));
    assert_eq!(result["truncated"], false);
}

#[test]
fn locate_search_hidden_file_option_preserves_visible_matches() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("visible.md"), "").expect("write visible");
    std::fs::write(dir.join(".hidden.md"), "").expect("write hidden");

    for include_hidden in [true, false] {
        let matches = run_locate_search(
            "md",
            dir.to_path_buf(),
            NonZero::new(10).expect("non-zero limit"),
            include_hidden,
        )
        .expect("locate search");
        assert!(
            matches
                .matches
                .iter()
                .any(|path| path.ends_with("visible.md")),
            "include_hidden={include_hidden}: {matches:?}"
        );
        assert_eq!(
            matches
                .matches
                .iter()
                .any(|path| path.ends_with(".hidden.md")),
            include_hidden,
            "include_hidden={include_hidden}: {matches:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn locate_search_skips_broken_symlinks() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("visible.md"), "").expect("write visible");
    symlink(dir.join("missing-target"), dir.join("broken-link"))
        .expect("create broken symlink");

    let matches = run_locate_search(
        "visible",
        dir.to_path_buf(),
        NonZero::new(10).expect("non-zero limit"),
        true,
    )
    .expect("broken symlink must not fail the search");

    assert!(
        matches.matches.iter().any(|path| path.ends_with("visible.md")),
        "matches: {matches:?}"
    );
}

#[tokio::test]
async fn execute_params_reports_truncated_matches() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let dir = temp.path();
    std::fs::write(dir.join("one.md"), "").expect("write one");
    std::fs::write(dir.join("two.md"), "").expect("write two");

    let output = execute_params(LocateFilesParams {
        pattern: "md".to_string(),
        path: Some(dir.to_string_lossy().into_owned()),
        limit: 1,
        include_hidden: true,
    })
    .await
    .expect("execute");

    assert!(
        output.contains("more matches not shown"),
        "output: {output}"
    );
}

#[tokio::test]
async fn verify_search_path_rejects_files() {
    let temp = tempfile::tempdir().expect("create temp dir");
    let file = temp.path().join("README.md");
    std::fs::write(&file, "").expect("write file");

    let err = verify_search_path(&file).await.expect_err("file rejected");
    assert!(
        err.contains("path must be a directory"),
        "unexpected error: {err}"
    );
}
