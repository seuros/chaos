use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

fn git(cwd: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_output(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output should be utf-8")
        .trim()
        .to_string()
}

struct ReviewDiffRepo {
    _repo: TempDir,
    cwd: std::path::PathBuf,
    base_sha: String,
    commit_sha: String,
}

fn repo_with_reviewable_commit(attributes: Option<&str>) -> ReviewDiffRepo {
    let repo = tempfile::tempdir().expect("create temp repo");
    let cwd = repo.path().to_path_buf();
    git(&cwd, &["init"]);
    git(&cwd, &["config", "user.email", "review@example.com"]);
    git(&cwd, &["config", "user.name", "Review Test"]);
    git(&cwd, &["config", "commit.gpgsign", "false"]);

    if let Some(attributes) = attributes {
        fs::write(cwd.join(".gitattributes"), attributes).expect("write attributes");
    }
    fs::write(cwd.join("tracked.txt"), "before\n").expect("write tracked file");
    git(&cwd, &["add", "."]);
    git(&cwd, &["commit", "-m", "initial"]);
    let base_sha = git_output(&cwd, &["rev-parse", "HEAD"]);

    fs::write(cwd.join("tracked.txt"), "after\n").expect("modify tracked file");
    git(&cwd, &["add", "tracked.txt"]);
    git(&cwd, &["commit", "-m", "change tracked file"]);
    let commit_sha = git_output(&cwd, &["rev-parse", "HEAD"]);

    ReviewDiffRepo {
        _repo: repo,
        cwd,
        base_sha,
        commit_sha,
    }
}

fn assert_has_tracked_file_diff(diff: &str) {
    assert!(
        diff.contains("-before\n"),
        "diff should include removed line: {diff}"
    );
    assert!(
        diff.contains("+after\n"),
        "diff should include added line: {diff}"
    );
}

async fn assert_prefetches_do_not_run_helper(repo: &ReviewDiffRepo, marker: &Path) {
    let base_branch = ReviewTarget::BaseBranch {
        branch: "main".to_string(),
    };
    let diff = fetch_review_diff(&base_branch, &repo.cwd, Some(&repo.base_sha))
        .await
        .expect("base branch diff should be fetched");
    assert_has_tracked_file_diff(&diff);
    assert!(
        !marker.exists(),
        "base branch review prefetch must not execute local diff helpers"
    );

    let commit = ReviewTarget::Commit {
        sha: repo.commit_sha.clone(),
        title: None,
    };
    let diff = fetch_review_diff(&commit, &repo.cwd, None)
        .await
        .expect("commit diff should be fetched");
    assert_has_tracked_file_diff(&diff);
    assert!(
        !marker.exists(),
        "commit review prefetch must not execute local diff helpers"
    );
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write helper");
    let mut permissions = fs::metadata(path).expect("stat helper").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod helper");
}

#[tokio::test]
async fn review_diff_prefetch_disables_external_diff_helpers() {
    let repo = repo_with_reviewable_commit(None);
    let marker = repo.cwd.join("external-diff-ran");
    let marker_arg = shell_quote(&marker.display().to_string());
    let helper = repo.cwd.join("external-diff-helper.sh");
    write_executable(
        &helper,
        &format!("#!/bin/sh\necho external-diff >> {marker_arg}\n"),
    );
    git(
        &repo.cwd,
        &["config", "diff.external", &helper.display().to_string()],
    );

    assert_prefetches_do_not_run_helper(&repo, &marker).await;
}

#[tokio::test]
async fn review_diff_prefetch_disables_textconv_helpers() {
    let repo = repo_with_reviewable_commit(Some("tracked.txt diff=reviewtextconv\n"));
    let marker = repo.cwd.join("textconv-ran");
    let marker_arg = shell_quote(&marker.display().to_string());
    let helper = repo.cwd.join("textconv-helper.sh");
    write_executable(
        &helper,
        &format!("#!/bin/sh\necho textconv >> {marker_arg}\ncat \"$1\"\n"),
    );
    git(
        &repo.cwd,
        &[
            "config",
            "diff.reviewtextconv.textconv",
            &helper.display().to_string(),
        ],
    );

    assert_prefetches_do_not_run_helper(&repo, &marker).await;
}

#[tokio::test]
async fn commit_diff_ref_starting_with_dash_is_not_treated_as_git_option() {
    let repo = tempfile::tempdir().expect("create temp repo");
    git(repo.path(), &["init"]);

    let tracked = repo.path().join("tracked.txt");
    fs::write(&tracked, "before\n").expect("write tracked file");
    git(repo.path(), &["add", "tracked.txt"]);
    fs::write(&tracked, "after\n").expect("modify tracked file");

    let output_path = repo.path().join("review-diff-output");
    let parent_output_path = repo.path().join("review-diff-output^");
    let target = ReviewTarget::Commit {
        sha: format!("--output={}", output_path.display()),
        title: None,
    };

    let diff = fetch_review_diff(&target, &repo.path().to_path_buf(), None).await;

    assert!(diff.is_none());
    assert!(
        !output_path.exists(),
        "commit sha must not be interpreted as git --output"
    );
    assert!(
        !parent_output_path.exists(),
        "parent ref must not be interpreted as git --output"
    );
}
