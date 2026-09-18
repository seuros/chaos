use std::fs;
use std::process::Command;

use tempfile::tempdir;

use super::*;

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn parses_branch_resource_defaults_and_percent_decoding() {
    let defaults = parse_branch_resource_uri("git://branches").expect("default URI");
    assert!(matches!(defaults.scope, BranchScope::All));
    assert!(defaults.contains.is_none());

    let filtered = parse_branch_resource_uri("git://branches?scope=local&contains=feature%2F")
        .expect("filtered URI");
    assert!(matches!(filtered.scope, BranchScope::Local));
    assert_eq!(filtered.contains.as_deref(), Some("feature/"));
}

#[test]
fn rejects_unknown_duplicate_and_invalid_query_parameters() {
    assert!(parse_branch_resource_uri("git://branches?scope=other").is_err());
    assert!(parse_branch_resource_uri("git://branches?scope=all&scope=local").is_err());
    assert!(parse_branch_resource_uri("git://branches?unexpected=true").is_err());
}

#[test]
fn branch_resource_applies_scope_and_contains_filters() {
    let temp = tempdir().expect("tempdir");
    git(temp.path(), &["init", "-b", "main"]);
    git(temp.path(), &["config", "user.name", "Test User"]);
    git(temp.path(), &["config", "user.email", "test@example.com"]);
    fs::write(temp.path().join("file.txt"), "initial\n").expect("write file");
    git(temp.path(), &["add", "file.txt"]);
    git(temp.path(), &["commit", "-m", "initial"]);
    git(temp.path(), &["branch", "feature/local"]);
    git(temp.path(), &["branch", "bugfix"]);
    git(
        temp.path(),
        &["update-ref", "refs/remotes/origin/feature/remote", "HEAD"],
    );

    let result = read_branches(
        temp.path(),
        BranchResourceParams {
            scope: BranchScope::Local,
            contains: Some("feature/".to_string()),
        },
    )
    .expect("read branches");
    assert!(
        !result.text.contains('\n'),
        "model-facing JSON must be compact"
    );
    let value: serde_json::Value = serde_json::from_str(&result.text).expect("parse resource JSON");
    assert_eq!(value["local"], serde_json::json!(["feature/local"]));
    assert_eq!(value["remote"], serde_json::json!([]));
    assert_eq!(value["current"], "main");
}
