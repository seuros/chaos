use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::tempdir;

use crate::DiffFormat;
use crate::DiffScope;
use crate::GitError;
use crate::diff_report;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("failed to run git");
    assert!(
        status.success(),
        "git command failed: git {}",
        args.join(" ")
    );
}

fn init_repo(dir: &Path) {
    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
}

#[test]
fn diff_report_returns_scoped_formats_and_whitespace_checks() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();
    init_repo(dir);

    let file = dir.join("file.txt");
    fs::write(&file, "one\ntwo\n").expect("write initial file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    fs::write(&file, "one\nstaged\n").expect("write staged file");
    git(dir, &["add", "file.txt"]);
    fs::write(&file, "one\nworktree  \n").expect("write worktree file");

    let staged = diff_report(
        dir,
        DiffScope::Staged,
        DiffFormat::Patch,
        None,
        Some(&["file.txt"]),
        false,
    )
    .expect("staged patch");
    let staged_patch = &staged.files[0].patch;
    assert!(staged_patch.contains("--- a/file.txt"));
    assert!(staged_patch.contains("+++ b/file.txt"));
    assert!(staged_patch.contains("-two"));
    assert!(staged_patch.contains("+staged"));
    assert!(!staged_patch.contains("worktree"));

    let worktree = diff_report(
        dir,
        DiffScope::Worktree,
        DiffFormat::Stat,
        None,
        None,
        false,
    )
    .expect("worktree stat");
    assert_eq!(worktree.summary.files_changed, 1);
    assert_eq!(worktree.files[0].path, "file.txt");
    assert_eq!(worktree.files[0].additions, Some(1));
    assert_eq!(worktree.files[0].deletions, Some(1));

    let all = diff_report(dir, DiffScope::All, DiffFormat::NameOnly, None, None, true)
        .expect("all changed paths");
    assert_eq!(all.paths, vec!["file.txt".to_string()]);
    assert_eq!(all.whitespace_errors[0].kind, "trailing_whitespace");
    assert_eq!(all.whitespace_errors[0].line, 2);

    let error = diff_report(
        dir,
        DiffScope::Worktree,
        DiffFormat::NameOnly,
        Some("HEAD"),
        None,
        false,
    )
    .expect_err("worktree base must be rejected");
    assert!(matches!(error, GitError::InvalidInput(_)));
    assert!(
        error
            .to_string()
            .contains("base cannot be used with worktree scope")
    );
}

#[test]
fn diff_report_name_only_skips_oversized_blob_content() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();
    git(dir, &["init"]);

    let large = vec![b'x'; 9 * 1024 * 1024];
    fs::write(dir.join("generated.txt"), large).expect("write large file");
    git(dir, &["add", "generated.txt"]);

    let names = diff_report(
        dir,
        DiffScope::Staged,
        DiffFormat::NameOnly,
        None,
        None,
        false,
    )
    .expect("name-only should not load blob content");
    assert_eq!(names.paths, vec!["generated.txt".to_string()]);
    assert!(names.summary.insertions.is_none());

    let error = diff_report(dir, DiffScope::Staged, DiffFormat::Patch, None, None, false)
        .expect_err("patch generation must reject oversized content");
    assert!(matches!(error, GitError::DiffLimit(_)));
    assert!(error.to_string().contains("generated.txt"));
}

#[test]
fn diff_report_all_filters_staged_changes_undone_in_worktree() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();
    init_repo(dir);

    fs::write(dir.join("file.txt"), "head\n").expect("write initial file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    fs::write(dir.join("file.txt"), "staged\n").expect("write staged content");
    git(dir, &["add", "file.txt"]);
    fs::write(dir.join("file.txt"), "head\n").expect("restore head content");

    let all = diff_report(dir, DiffScope::All, DiffFormat::NameOnly, None, None, false)
        .expect("all diff");
    assert!(all.paths.is_empty());
    assert_eq!(all.summary.files_changed, 0);
}

#[test]
fn diff_renders_unified_patch_against_head() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();
    init_repo(dir);

    fs::write(dir.join("file.txt"), "one\n").expect("write initial file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);
    fs::write(dir.join("file.txt"), "one\ntwo\n").expect("write change");

    let patch = crate::diff(dir, None, None).expect("diff");
    assert!(patch.contains("+two"));
    assert_eq!(crate::status(dir).expect("status").unstaged.len(), 1);
    assert_eq!(crate::log(dir, Some(1), None).expect("log").len(), 1);
}
