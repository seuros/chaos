//! Public-API tests for `chaos-scm` — driving real git processes over
//! real repositories and watching what shakes loose.
//!
//! These tests shell out to `git(1)` against scratch tempdirs, so they
//! exercise the actual diff-application pipeline end-to-end: successful
//! adds, conflicting modifies, missing-index rejections, forward+revert
//! roundtrips, preflight atomicity, and the branch-mergebase helper
//! that has to guess right about upstream vs. local refs. Order comes
//! out of chaos; these tests make sure the order is the right one.

// Test-setup helpers (init_repo, run_git, read_normalized) genuinely
// should panic if the local git or filesystem can't cooperate — there's
// nothing meaningful to test if the harness itself collapses.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::sync::OnceLock;

use chaos_scm::ApplyGitRequest;
use chaos_scm::GitToolingError;
use chaos_scm::apply_git_patch;
use chaos_scm::extract_paths_from_patch;
use chaos_scm::merge_base_with_head;
use chaos_scm::parse_git_apply_output;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tempfile::tempdir;

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn run_git<I, S>(repo: &Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("git command");
    assert!(status.success(), "git command failed");
}

fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git command");
    assert!(output.status.success(), "git command failed: {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo_with_identity() -> TempDir {
    let dir = tempdir().expect("tempdir");
    run_git(dir.path(), ["init", "--initial-branch=main"]);
    run_git(dir.path(), ["config", "core.autocrlf", "false"]);
    run_git(dir.path(), ["config", "user.name", "Chaos"]);
    run_git(dir.path(), ["config", "user.email", "chaos@example.com"]);
    dir
}

fn commit_all(repo: &Path, message: &str) {
    run_git(repo, ["add", "-A"]);
    run_git(repo, ["commit", "-m", message]);
}

fn read_normalized(path: &Path) -> String {
    std::fs::read_to_string(path)
        .expect("read file")
        .replace("\r\n", "\n")
}

// ── patch parsing ──────────────────────────────────────────────────────

#[test]
fn patch_header_parser_handles_quoting_unescaping_and_dev_null() {
    // Quoted filenames with spaces must survive round-trip.
    let quoted_space = "diff --git \"a/hello world.txt\" \"b/hello world.txt\"\nnew file mode 100644\n--- /dev/null\n+++ b/hello world.txt\n@@ -0,0 +1 @@\n+hi\n";
    assert_eq!(
        extract_paths_from_patch(quoted_space),
        vec!["hello world.txt".to_string()]
    );

    // The `/dev/null` sentinel on the `---` side must not be reported
    // as a real path.
    let dev_null = "diff --git a/dev/null b/ok.txt\nnew file mode 100644\n--- /dev/null\n+++ b/ok.txt\n@@ -0,0 +1 @@\n+hi\n";
    assert_eq!(
        extract_paths_from_patch(dev_null),
        vec!["ok.txt".to_string()]
    );

    // C-style escapes inside quoted headers must decode to the raw byte.
    let escaped = "diff --git \"a/hello\\tworld.txt\" \"b/hello\\tworld.txt\"\nnew file mode 100644\n--- /dev/null\n+++ b/hello\tworld.txt\n@@ -0,0 +1 @@\n+hi\n";
    assert_eq!(
        extract_paths_from_patch(escaped),
        vec!["hello\tworld.txt".to_string()]
    );

    // The stderr parser unescapes quoted paths it reports as skipped.
    let (applied, skipped, conflicted) =
        parse_git_apply_output("", "error: patch failed: \"hello\\tworld.txt\":1\n");
    assert_eq!(applied, Vec::<String>::new());
    assert_eq!(conflicted, Vec::<String>::new());
    assert_eq!(skipped, vec!["hello\tworld.txt".to_string()]);
}

// ── apply success / conflict / missing index ────────────────────────────

#[test]
fn apply_add_creates_file_in_worktree() {
    let _g = env_lock().lock().unwrap();
    let repo = init_repo_with_identity();
    let root = repo.path();

    let diff = "diff --git a/hello.txt b/hello.txt\nnew file mode 100644\n--- /dev/null\n+++ b/hello.txt\n@@ -0,0 +1,2 @@\n+hello\n+world\n";
    let r = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: false,
        preflight: false,
    })
    .expect("run apply");
    assert_eq!(r.exit_code, 0);
    assert!(root.join("hello.txt").exists());
}

#[test]
fn apply_modify_fails_on_conflict_and_on_missing_index() {
    let _g = env_lock().lock().unwrap();

    // Conflict: the patch touches the same line the worktree already
    // edited locally.
    let conflict_repo = init_repo_with_identity();
    let conflict_root = conflict_repo.path();
    std::fs::write(conflict_root.join("file.txt"), "line1\nline2\nline3\n").unwrap();
    commit_all(conflict_root, "seed");
    std::fs::write(conflict_root.join("file.txt"), "line1\nlocal2\nline3\n").unwrap();

    let diff = "diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n line1\n-line2\n+remote2\n line3\n";
    let r = apply_git_patch(&ApplyGitRequest {
        cwd: conflict_root.to_path_buf(),
        diff: diff.to_string(),
        revert: false,
        preflight: false,
    })
    .expect("run apply");
    assert_ne!(r.exit_code, 0, "conflict must produce non-zero exit");

    // Missing index: the patch targets a file that git has never seen.
    let ghost_repo = init_repo_with_identity();
    let ghost_root = ghost_repo.path();
    let ghost_diff = "diff --git a/ghost.txt b/ghost.txt\n--- a/ghost.txt\n+++ b/ghost.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";
    let r = apply_git_patch(&ApplyGitRequest {
        cwd: ghost_root.to_path_buf(),
        diff: ghost_diff.to_string(),
        revert: false,
        preflight: false,
    })
    .expect("run apply");
    assert_ne!(r.exit_code, 0, "missing-index must produce non-zero exit");
}

#[test]
fn apply_then_revert_roundtrips_the_worktree() {
    let _g = env_lock().lock().unwrap();
    let repo = init_repo_with_identity();
    let root = repo.path();

    std::fs::write(root.join("file.txt"), "orig\n").unwrap();
    commit_all(root, "seed");

    let diff = "diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1,1 +1,1 @@\n-orig\n+ORIG\n";
    let apply = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: false,
        preflight: false,
    })
    .expect("apply ok");
    assert_eq!(apply.exit_code, 0);
    assert_eq!(read_normalized(&root.join("file.txt")), "ORIG\n");

    let revert = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: true,
        preflight: false,
    })
    .expect("revert ok");
    assert_eq!(revert.exit_code, 0);
    assert_eq!(read_normalized(&root.join("file.txt")), "orig\n");
}

#[test]
fn revert_preflight_runs_check_without_staging_anything() {
    let _g = env_lock().lock().unwrap();
    let repo = init_repo_with_identity();
    let root = repo.path();

    std::fs::write(root.join("file.txt"), "orig\n").unwrap();
    commit_all(root, "seed");

    let diff = "diff --git a/file.txt b/file.txt\n--- a/file.txt\n+++ b/file.txt\n@@ -1,1 +1,1 @@\n-orig\n+ORIG\n";
    let apply = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: false,
        preflight: false,
    })
    .expect("forward apply ok");
    assert_eq!(apply.exit_code, 0);
    run_git(root, ["commit", "-am", "apply change"]);

    let staged_before = git_stdout(root, &["diff", "--cached", "--name-only"]);
    let preflight = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: true,
        preflight: true,
    })
    .expect("preflight ok");
    assert_eq!(preflight.exit_code, 0);
    let staged_after = git_stdout(root, &["diff", "--cached", "--name-only"]);
    assert_eq!(
        staged_after.trim(),
        staged_before.trim(),
        "preflight must not stage any paths"
    );
    assert_eq!(read_normalized(&root.join("file.txt")), "ORIG\n");
}

#[test]
fn preflight_is_atomic_and_records_check_flag_in_log() {
    let _g = env_lock().lock().unwrap();
    let repo = init_repo_with_identity();
    let root = repo.path();

    // Multi-file diff: ok.txt could add cleanly, ghost.txt can't modify.
    // With preflight, neither must land; without it, the engine reports
    // failure but must not record `--check`.
    let diff = "diff --git a/ok.txt b/ok.txt\nnew file mode 100644\n--- /dev/null\n+++ b/ok.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n\ndiff --git a/ghost.txt b/ghost.txt\n--- a/ghost.txt\n+++ b/ghost.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n";

    let pre = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: false,
        preflight: true,
    })
    .expect("preflight run");
    assert_ne!(pre.exit_code, 0);
    assert!(
        !root.join("ok.txt").exists(),
        "preflight must not write ok.txt when the whole patch is rejected"
    );
    assert!(pre.cmd_for_log.contains("--check"));

    let direct = apply_git_patch(&ApplyGitRequest {
        cwd: root.to_path_buf(),
        diff: diff.to_string(),
        revert: false,
        preflight: false,
    })
    .expect("direct run");
    assert_ne!(direct.exit_code, 0);
    assert!(!direct.cmd_for_log.contains("--check"));
}

// ── merge-base against HEAD ─────────────────────────────────────────────

#[test]
fn merge_base_finds_shared_commit_between_branches() -> Result<(), GitToolingError> {
    let temp = init_repo_with_identity();
    let repo = temp.path();

    std::fs::write(repo.join("base.txt"), "base\n")?;
    commit_all(repo, "base commit");

    run_git(repo, ["checkout", "-b", "feature"]);
    std::fs::write(repo.join("feature.txt"), "feature change\n")?;
    commit_all(repo, "feature commit");

    run_git(repo, ["checkout", "main"]);
    std::fs::write(repo.join("main.txt"), "main change\n")?;
    commit_all(repo, "main commit");

    run_git(repo, ["checkout", "feature"]);

    let expected = git_stdout(repo, &["merge-base", "HEAD", "main"]);
    assert_eq!(merge_base_with_head(repo, "main")?, Some(expected));
    Ok(())
}

#[test]
fn merge_base_prefers_remote_tracking_when_remote_has_moved_past_local()
-> Result<(), GitToolingError> {
    let temp = tempdir()?;
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    std::fs::create_dir_all(&repo)?;
    std::fs::create_dir_all(&remote)?;

    run_git(&remote, ["init", "--bare"]);
    run_git(&repo, ["init", "--initial-branch=main"]);
    run_git(&repo, ["config", "core.autocrlf", "false"]);
    run_git(&repo, ["config", "user.name", "Chaos"]);
    run_git(&repo, ["config", "user.email", "chaos@example.com"]);

    std::fs::write(repo.join("base.txt"), "base\n")?;
    commit_all(&repo, "base commit");

    run_git(
        &repo,
        [
            "remote",
            "add",
            "origin",
            remote.to_str().expect("utf8 path"),
        ],
    );
    run_git(&repo, ["push", "-u", "origin", "main"]);

    run_git(&repo, ["checkout", "-b", "feature"]);
    std::fs::write(repo.join("feature.txt"), "feature change\n")?;
    commit_all(&repo, "feature commit");

    // Rewrite local main so the remote-tracking ref is strictly ahead
    // of local main. The helper must prefer `origin/main` in that case.
    run_git(&repo, ["checkout", "--orphan", "rewrite"]);
    run_git(&repo, ["rm", "-rf", "."]);
    std::fs::write(repo.join("new-main.txt"), "rewritten main\n")?;
    commit_all(&repo, "rewrite main");
    run_git(&repo, ["branch", "-M", "rewrite", "main"]);
    run_git(&repo, ["branch", "--set-upstream-to=origin/main", "main"]);

    run_git(&repo, ["checkout", "feature"]);
    run_git(&repo, ["fetch", "origin"]);

    let expected = git_stdout(&repo, &["merge-base", "HEAD", "origin/main"]);
    assert_eq!(merge_base_with_head(&repo, "main")?, Some(expected));
    Ok(())
}

#[test]
fn merge_base_returns_none_when_target_branch_is_missing() -> Result<(), GitToolingError> {
    let temp = init_repo_with_identity();
    let repo = temp.path();
    std::fs::write(repo.join("tracked.txt"), "tracked\n")?;
    commit_all(repo, "initial");

    assert_eq!(merge_base_with_head(repo, "missing-branch")?, None);
    Ok(())
}
