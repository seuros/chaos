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

fn init_repo(dir: &Path) {
    git(dir, &["init", "-b", "main"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    fs::write(dir.join("file.txt"), "initial\n").expect("write file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);
}

#[test]
fn create_does_not_checkout_and_delete_removes_merged_branch() {
    let temp = tempdir().expect("tempdir");
    init_repo(temp.path());

    let created = create(temp.path(), "topic", None).expect("create branch");
    assert_eq!(created.operation, "create");
    assert_eq!(
        crate::branches(temp.path())
            .expect("branches")
            .current
            .as_deref(),
        Some("main")
    );
    assert!(
        crate::branches(temp.path())
            .expect("branches")
            .local
            .contains(&"topic".to_string())
    );

    let deleted = delete(temp.path(), "topic", false).expect("delete merged branch");
    assert_eq!(deleted.oid, created.oid);
    assert!(
        !crate::branches(temp.path())
            .expect("branches")
            .local
            .contains(&"topic".to_string())
    );
}

#[test]
fn delete_rejects_unmerged_branch_without_force() {
    let temp = tempdir().expect("tempdir");
    init_repo(temp.path());
    git(temp.path(), &["switch", "-c", "topic"]);
    fs::write(temp.path().join("topic.txt"), "topic\n").expect("write topic");
    git(temp.path(), &["add", "topic.txt"]);
    git(temp.path(), &["commit", "-m", "topic"]);
    git(temp.path(), &["switch", "main"]);

    let error = delete(temp.path(), "topic", false).expect_err("reject unmerged");
    assert!(error.to_string().contains("not fully merged"));
    delete(temp.path(), "topic", true).expect("force delete");
}

#[test]
fn delete_rejects_branch_checked_out_in_linked_worktree() {
    let temp = tempdir().expect("tempdir");
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    fs::create_dir(&repo).expect("create repo");
    init_repo(&repo);
    create(&repo, "topic", None).expect("create topic");
    git(
        &repo,
        &[
            "worktree",
            "add",
            worktree.to_str().expect("utf8 worktree"),
            "topic",
        ],
    );

    let error = delete(&repo, "topic", true).expect_err("reject checked out branch");
    assert!(error.to_string().contains("checked out"));
}

#[test]
fn rejects_full_reference_names() {
    let temp = tempdir().expect("tempdir");
    init_repo(temp.path());
    let error = create(temp.path(), "refs/heads/topic", None).expect_err("reject full name");
    assert!(error.to_string().contains("short local name"));
}
