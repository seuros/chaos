use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tempfile::tempdir;

use super::GitAddParams;
use super::GitBlameParams;
use super::GitCommitParams;
use super::GitDiffParams;
use super::GitShowFileParams;
use super::GitShowParams;
use super::execute_cancellable_blocking;
use super::execute_git_add_structured;
use super::execute_git_blame_structured;
use super::execute_git_commit_structured;
use super::execute_git_diff_structured_with_cancel;
use super::execute_git_show_file_structured;
use super::execute_git_show_structured;
use crate::BlameLine;
use crate::CommitTrailer;
use crate::DiffFormat;
use crate::DiffScope;
use crate::FileAtRev;
use crate::ShowEntry;
fn execute_git_diff_structured(
    cwd: &Path,
    params: GitDiffParams,
) -> Result<serde_json::Value, String> {
    execute_git_diff_structured_with_cancel(cwd, params, Arc::new(AtomicBool::new(false)))
}

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

fn git_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("failed to run git");
    assert!(
        output.status.success(),
        "git command failed: git {}\nstderr: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git output is utf8")
}

#[test]
fn execute_git_diff_returns_scoped_structured_formats_and_checks() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    let file = dir.join("file.txt");
    fs::write(&file, "one\ntwo\n").expect("write initial file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    fs::write(&file, "one\nstaged\n").expect("write staged file");
    git(dir, &["add", "file.txt"]);
    fs::write(&file, "one\nworktree  \n").expect("write worktree file");

    let staged = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::Staged,
            format: DiffFormat::Patch,
            check: false,
            base: None,
            paths: Some(vec!["file.txt".to_string()]),
        },
    )
    .expect("staged patch");
    let staged_patch = staged["result"]["files"][0]["patch"]
        .as_str()
        .expect("staged patch text");
    assert!(staged_patch.contains("--- a/file.txt"));
    assert!(staged_patch.contains("+++ b/file.txt"));
    assert!(staged_patch.contains("-two"));
    assert!(staged_patch.contains("+staged"));
    assert!(!staged_patch.contains("worktree"));

    let worktree = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::Worktree,
            format: DiffFormat::Stat,
            check: false,
            base: None,
            paths: None,
        },
    )
    .expect("worktree stat");
    assert_eq!(worktree["scope"], "worktree");
    assert_eq!(worktree["summary"]["files_changed"], 1);
    assert_eq!(worktree["result"]["format"], "stat");
    assert_eq!(worktree["result"]["files"][0]["path"], "file.txt");
    assert_eq!(worktree["result"]["files"][0]["additions"], 1);
    assert_eq!(worktree["result"]["files"][0]["deletions"], 1);

    let all = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::All,
            format: DiffFormat::NameOnly,
            check: true,
            base: None,
            paths: None,
        },
    )
    .expect("all changed paths");
    assert_eq!(all["base"], "HEAD");
    assert_eq!(all["result"]["format"], "name_only");
    assert_eq!(all["result"]["paths"], serde_json::json!(["file.txt"]));
    assert_eq!(all["whitespace_check"]["passed"], false);
    assert_eq!(
        all["whitespace_check"]["errors"][0]["kind"],
        "trailing_whitespace"
    );
    assert_eq!(all["whitespace_check"]["errors"][0]["line"], 2);

    let error = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::Worktree,
            format: DiffFormat::NameOnly,
            check: false,
            base: Some("HEAD".to_string()),
            paths: None,
        },
    )
    .expect_err("worktree base must be rejected");
    assert!(error.contains("base cannot be used with worktree scope"));
}

#[test]
fn execute_git_diff_name_only_skips_oversized_blob_content() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    let large = vec![b'x'; 9 * 1024 * 1024];
    fs::write(dir.join("generated.txt"), large).expect("write large file");
    git(dir, &["add", "generated.txt"]);

    let names = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::Staged,
            format: DiffFormat::NameOnly,
            check: false,
            base: None,
            paths: None,
        },
    )
    .expect("name-only should not load blob content");
    assert_eq!(
        names["result"]["paths"],
        serde_json::json!(["generated.txt"])
    );
    assert!(
        !names["summary"]
            .as_object()
            .expect("summary object")
            .contains_key("insertions")
    );

    let error = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::Staged,
            format: DiffFormat::Patch,
            check: false,
            base: None,
            paths: None,
        },
    )
    .expect_err("patch generation must reject oversized content");
    assert!(error.contains("per-file limit"));
    assert!(error.contains("generated.txt"));
}

#[test]
fn execute_git_diff_all_filters_staged_changes_undone_in_worktree() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    fs::write(dir.join("file.txt"), "head\n").expect("write initial file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    fs::write(dir.join("file.txt"), "staged\n").expect("write staged content");
    git(dir, &["add", "file.txt"]);
    fs::write(dir.join("file.txt"), "head\n").expect("restore head content");

    let all = execute_git_diff_structured(
        dir,
        GitDiffParams {
            scope: DiffScope::All,
            format: DiffFormat::NameOnly,
            check: false,
            base: None,
            paths: None,
        },
    )
    .expect("all diff");
    assert_eq!(all["result"]["paths"], serde_json::json!([]));
    assert_eq!(all["summary"]["files_changed"], 0);
}

#[test]
fn cancellable_blocking_waits_for_worker_shutdown_after_timeout() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    let stopped = Arc::new(AtomicBool::new(false));
    let worker_stopped = Arc::clone(&stopped);

    let result = runtime.block_on(execute_cancellable_blocking(
        PathBuf::from("."),
        (),
        Duration::from_millis(20),
        "test operation",
        move |_, (), cancel| {
            while !cancel.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            worker_stopped.store(true, Ordering::Release);
            Ok::<(), String>(())
        },
    ));

    assert_eq!(
        result.expect_err("operation must time out"),
        "test operation timed out after 20ms"
    );
    assert!(
        stopped.load(Ordering::Acquire),
        "timeout must not leave blocking work running"
    );
}

#[test]
fn execute_git_blame_includes_head_author_placeholder() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    let file = dir.join("file.txt");
    fs::write(&file, "alpha\nbeta\n").expect("write file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    let blame_json = execute_git_blame_structured(
        dir,
        GitBlameParams {
            file_path: "file.txt".to_string(),
            start_line: Some(1),
            end_line: Some(1),
        },
    )
    .expect("blame");

    let blamed: Vec<BlameLine> = serde_json::from_value(blame_json).expect("parse blame json");
    assert_eq!(blamed.len(), 1);
    assert_eq!(blamed[0].author, "Test User");
    assert_eq!(blamed[0].content, "alpha");
    assert!(!blamed[0].sha.is_empty());
}

#[test]
fn execute_git_show_returns_subject_body_and_trailers() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    let file = dir.join("file.txt");
    fs::write(&file, "alpha\n").expect("write file");
    git(dir, &["add", "file.txt"]);
    git(
        dir,
        &[
            "commit",
            "-m",
            "feat: roast engine online",
            "-m",
            "Claude wrote a commit body with all the charisma of a tax form.\n\nSigned-off-by: Test User <test@example.com>",
        ],
    );

    let show_json = execute_git_show_structured(
        dir,
        GitShowParams {
            rev: Some("HEAD".to_string()),
        },
    )
    .expect("show");

    let shown: ShowEntry = serde_json::from_value(show_json).expect("parse show json");
    assert_eq!(shown.subject, "feat: roast engine online");
    assert!(shown.body.contains("charisma of a tax form"));
    assert_eq!(shown.author, "Test User");
    assert_eq!(shown.trailers.len(), 1);
    assert_eq!(shown.trailers[0].token, "Signed-off-by");
    assert_eq!(shown.trailers[0].value, "Test User <test@example.com>");
}

#[test]
fn execute_git_show_file_reads_revision_not_worktree() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    fs::create_dir(dir.join("src")).expect("create src");
    fs::write(dir.join("src/file.txt"), "alpha\nbeta\ngamma\n").expect("write file");
    git(dir, &["add", "src/file.txt"]);
    git(dir, &["commit", "-m", "initial"]);
    let head = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();

    fs::write(dir.join("src/file.txt"), "worktree\n").expect("dirty worktree");

    let shown_json = execute_git_show_file_structured(
        dir,
        GitShowFileParams {
            file_path: "src/file.txt".to_string(),
            rev: None,
            start_line: None,
            end_line: None,
        },
    )
    .expect("show file");
    let shown: FileAtRev = serde_json::from_value(shown_json).expect("parse show file json");
    assert_eq!(shown.path, "src/file.txt");
    assert_eq!(shown.rev, "HEAD");
    assert_eq!(shown.sha, head);
    assert_eq!(shown.content, "alpha\nbeta\ngamma\n");
    assert_eq!(shown.total_lines, 3);
    assert_eq!(shown.start_line, None);
    assert_eq!(shown.end_line, None);

    let ranged = execute_git_show_file_structured(
        dir,
        GitShowFileParams {
            file_path: "./src/file.txt".to_string(),
            rev: Some("HEAD".to_string()),
            start_line: Some(2),
            end_line: Some(3),
        },
    )
    .expect("show file range");
    assert_eq!(ranged["content"], "beta\ngamma\n");
    assert_eq!(ranged["start_line"], 2);
    assert_eq!(ranged["end_line"], 3);
    assert_eq!(ranged["total_lines"], 3);

    let missing = execute_git_show_file_structured(
        dir,
        GitShowFileParams {
            file_path: "missing.txt".to_string(),
            rev: None,
            start_line: None,
            end_line: None,
        },
    )
    .expect_err("missing file must fail");
    assert!(missing.contains("path not found: missing.txt"));

    let incomplete = execute_git_show_file_structured(
        dir,
        GitShowFileParams {
            file_path: "src/file.txt".to_string(),
            rev: None,
            start_line: Some(1),
            end_line: None,
        },
    )
    .expect_err("partial line range must fail");
    assert!(incomplete.contains("both be provided or both be omitted"));

    let directory = execute_git_show_file_structured(
        dir,
        GitShowFileParams {
            file_path: "src".to_string(),
            rev: None,
            start_line: None,
            end_line: None,
        },
    )
    .expect_err("directory must fail");
    assert!(directory.contains("path is a directory: src"));
}

#[test]
fn execute_git_add_and_commit_create_initial_and_followup_commits() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    let file = dir.join("file.txt");
    fs::write(&file, "alpha\n").expect("write file");

    let added = execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["file.txt".to_string()],
        },
    )
    .expect("add");
    assert_eq!(added["staged"], serde_json::json!(["file.txt"]));
    assert_eq!(added["removed"], serde_json::json!([]));

    let status = crate::status(dir).expect("status");
    assert_eq!(status.staged.len(), 1);
    assert_eq!(status.staged[0].path, "file.txt");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let hook = dir.join(".git/hooks/pre-commit");
        fs::create_dir_all(hook.parent().expect("hook parent")).expect("create hooks dir");
        fs::write(
            &hook,
            "#!/bin/sh\necho hook-ran > \"$PWD/hook-ran\"\nexit 1\n",
        )
        .expect("write hook");
        let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).expect("make hook executable");
    }

    let committed = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "initial commit".to_string(),
            trailers: vec![],
            amend: false,
        },
    )
    .expect("commit");
    assert_eq!(committed["operation"], "create");
    assert_eq!(committed["previous_sha"], serde_json::Value::Null);
    assert_eq!(committed["subject"], "initial commit");
    assert_eq!(committed["trailers"], serde_json::json!([]));
    assert_eq!(
        committed["committed_paths"],
        serde_json::json!(["file.txt"])
    );
    assert!(!committed["sha"].as_str().unwrap_or_default().is_empty());
    assert_eq!(git_output(dir, &["show", "HEAD:file.txt"]), "alpha\n");
    assert!(!dir.join("hook-ran").exists(), "Git hooks must not run");

    fs::write(&file, "beta\n").expect("modify tracked file");
    fs::write(dir.join("untracked.txt"), "leave me out\n").expect("write untracked file");

    execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["file.txt".to_string()],
        },
    )
    .expect("stage tracked file");
    execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "update tracked file".to_string(),
            trailers: vec![],
            amend: false,
        },
    )
    .expect("followup commit");

    assert_eq!(git_output(dir, &["show", "HEAD:file.txt"]), "beta\n");
    assert!(
        git_output(dir, &["status", "--porcelain"]).contains("?? untracked.txt"),
        "unrequested files must remain untracked"
    );

    let empty = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "empty".to_string(),
            trailers: vec![],
            amend: false,
        },
    )
    .expect_err("empty commit must fail");
    assert!(empty.contains("nothing staged to commit"));
}

#[test]
fn execute_git_commit_appends_structured_trailers_and_returns_them() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
    git(dir, &["add", "file.txt"]);

    let committed = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: " Implement commit trailers\n\nKeep the API structured. ".to_string(),
            trailers: vec![
                CommitTrailer {
                    token: " Signed-off-by ".to_string(),
                    value: " Mira Tenner <mira-agent@agentmail.to> ".to_string(),
                },
                CommitTrailer {
                    token: "Co-authored-by".to_string(),
                    value: "Daniel Tenner <daniel@tenner.org>".to_string(),
                },
            ],
            amend: false,
        },
    )
    .expect("commit with trailers");

    assert_eq!(committed["operation"], "create");
    assert_eq!(committed["previous_sha"], serde_json::Value::Null);
    assert_eq!(
        committed["trailers"],
        serde_json::json!([
            {
                "token": "Signed-off-by",
                "value": "Mira Tenner <mira-agent@agentmail.to>"
            },
            {
                "token": "Co-authored-by",
                "value": "Daniel Tenner <daniel@tenner.org>"
            }
        ])
    );
    assert_eq!(
        git_output(dir, &["show", "-s", "--format=%B", "HEAD"]),
        concat!(
            "Implement commit trailers\n\n",
            "Keep the API structured.\n\n",
            "Signed-off-by: Mira Tenner <mira-agent@agentmail.to>\n",
            "Co-authored-by: Daniel Tenner <daniel@tenner.org>\n"
        )
    );

    let shown = crate::show(dir, None).expect("show committed trailers");
    assert_eq!(
        shown.trailers,
        vec![
            CommitTrailer {
                token: "Signed-off-by".to_string(),
                value: "Mira Tenner <mira-agent@agentmail.to>".to_string(),
            },
            CommitTrailer {
                token: "Co-authored-by".to_string(),
                value: "Daniel Tenner <daniel@tenner.org>".to_string(),
            },
        ]
    );
}

#[test]
fn execute_git_commit_rejects_invalid_structured_trailers() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
    git(dir, &["add", "file.txt"]);

    let invalid = [
        (
            CommitTrailer {
                token: "Signed off by".to_string(),
                value: "Test User <test@example.com>".to_string(),
            },
            "invalid commit trailer token",
        ),
        (
            CommitTrailer {
                token: "Signed-off-by".to_string(),
                value: "Test User\nInjected-by: Someone".to_string(),
            },
            "must be a single line",
        ),
        (
            CommitTrailer {
                token: "Signed-off-by".to_string(),
                value: "   ".to_string(),
            },
            "must not be empty",
        ),
    ];

    for (trailer, expected) in invalid {
        let error = execute_git_commit_structured(
            dir,
            GitCommitParams {
                message: "invalid trailer".to_string(),
                trailers: vec![trailer],
                amend: false,
            },
        )
        .expect_err("invalid trailer must be rejected");
        assert!(error.contains(expected), "unexpected error: {error}");
    }

    assert_eq!(
        git_output(dir, &["rev-list", "--count", "--all"]),
        "0\n",
        "invalid trailers must not create a commit"
    );
}

#[test]
fn execute_git_add_stages_deletions_and_rejects_unsafe_paths() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    fs::write(dir.join("tracked.txt"), "tracked\n").expect("write tracked file");
    git(dir, &["add", "tracked.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    fs::remove_file(dir.join("tracked.txt")).expect("remove tracked file");
    let deleted = execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["tracked.txt".to_string()],
        },
    )
    .expect("stage deletion");
    assert_eq!(deleted["removed"], serde_json::json!(["tracked.txt"]));

    fs::write(dir.join(".gitignore"), "*.log\n").expect("write ignore file");
    fs::write(dir.join("ignored.log"), "ignored\n").expect("write ignored file");
    let ignored = execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["ignored.log".to_string()],
        },
    )
    .expect_err("ignored file must fail");
    assert!(ignored.contains("path is ignored: ignored.log"));

    let traversal = execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["../outside.txt".to_string()],
        },
    )
    .expect_err("path traversal must fail");
    assert!(traversal.contains("may not escape the repository"));

    let directory = execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec![".git".to_string()],
        },
    )
    .expect_err("git directory must fail");
    assert!(directory.contains("Git directory"));
}

#[test]
fn execute_git_add_paths_are_relative_to_repository_root() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    fs::create_dir(dir.join("nested")).expect("create nested directory");
    fs::write(dir.join("root.txt"), "root\n").expect("write root file");

    let added = execute_git_add_structured(
        &dir.join("nested"),
        GitAddParams {
            paths: vec!["root.txt".to_string()],
        },
    )
    .expect("stage root-relative path from nested cwd");

    assert_eq!(added["staged"], serde_json::json!(["root.txt"]));
    assert_eq!(
        git_output(dir, &["diff", "--cached", "--name-only"]),
        "root.txt\n"
    );
}

#[test]
fn execute_git_commit_preserves_unchanged_gitlinks_and_rejects_changes() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);

    fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);

    let gitlink_id = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();
    git(
        dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            &gitlink_id,
            "vendor/reference",
        ],
    );
    git(dir, &["commit", "-m", "add gitlink"]);

    fs::write(dir.join("file.txt"), "beta\n").expect("modify file");
    execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["file.txt".to_string()],
        },
    )
    .expect("stage file");
    execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "update file".to_string(),
            trailers: vec![],
            amend: false,
        },
    )
    .expect("commit while preserving gitlink");

    assert!(
        git_output(dir, &["ls-tree", "HEAD", "vendor/reference"]).contains(&gitlink_id),
        "unchanged gitlink must be preserved"
    );

    let changed_gitlink_id = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();
    git(
        dir,
        &[
            "update-index",
            "--cacheinfo",
            "160000",
            &changed_gitlink_id,
            "vendor/reference",
        ],
    );
    let error = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "change gitlink".to_string(),
            trailers: vec![],
            amend: false,
        },
    )
    .expect_err("staged gitlink change must fail");
    assert!(error.contains("staged submodule changes are not supported: vendor/reference"));
}

#[test]
fn execute_git_commit_amends_message_and_includes_staged_changes() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();

    git(dir, &["init"]);
    git(dir, &["config", "user.name", "Test User"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    fs::write(dir.join("file.txt"), "alpha\n").expect("write file");
    git(dir, &["add", "file.txt"]);
    git(dir, &["commit", "-m", "initial"]);
    fs::write(dir.join("second.txt"), "second\n").expect("write second file");
    git(dir, &["add", "second.txt"]);
    git(dir, &["commit", "-m", "second"]);

    let original_sha = git_output(dir, &["rev-parse", "HEAD"]).trim().to_string();
    let original_parent = git_output(dir, &["rev-parse", "HEAD^"]).trim().to_string();
    let original_author = git_output(dir, &["show", "-s", "--format=%an|%ae|%at", "HEAD"]);

    let message_only = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "second, revised".to_string(),
            trailers: vec![],
            amend: true,
        },
    )
    .expect("amend message");

    assert_eq!(message_only["operation"], "amend");
    assert_eq!(
        message_only["previous_sha"].as_str(),
        Some(original_sha.as_str())
    );
    assert_eq!(message_only["subject"], "second, revised");
    assert_eq!(message_only["trailers"], serde_json::json!([]));
    assert_eq!(message_only["committed_paths"], serde_json::json!([]));
    assert_ne!(
        message_only["sha"].as_str().expect("amended sha"),
        original_sha
    );
    assert_eq!(
        git_output(dir, &["rev-parse", "HEAD^"]).trim(),
        original_parent
    );
    assert_eq!(
        git_output(dir, &["show", "-s", "--format=%an|%ae|%at", "HEAD"]),
        original_author,
        "amend must preserve the original author"
    );
    assert_eq!(git_output(dir, &["rev-list", "--count", "HEAD"]), "2\n");

    fs::write(dir.join("second.txt"), "updated\n").expect("update second file");
    execute_git_add_structured(
        dir,
        GitAddParams {
            paths: vec!["second.txt".to_string()],
        },
    )
    .expect("stage amended content");
    let with_changes = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "second, revised again".to_string(),
            trailers: vec![],
            amend: true,
        },
    )
    .expect("amend with staged change");

    assert_eq!(
        with_changes["committed_paths"],
        serde_json::json!(["second.txt"])
    );
    assert_eq!(git_output(dir, &["show", "HEAD:second.txt"]), "updated\n");
    assert_eq!(
        git_output(dir, &["rev-parse", "HEAD^"]).trim(),
        original_parent
    );
    assert_eq!(git_output(dir, &["rev-list", "--count", "HEAD"]), "2\n");

    git(dir, &["checkout", "--detach", "HEAD"]);
    let detached = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "detached revision".to_string(),
            trailers: vec![],
            amend: true,
        },
    )
    .expect("amend detached HEAD");
    assert_eq!(detached["detached"], true);
    assert_eq!(detached["branch"], serde_json::Value::Null);
    assert_eq!(
        git_output(dir, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "HEAD\n"
    );
    assert_eq!(
        git_output(dir, &["rev-parse", "HEAD^"]).trim(),
        original_parent
    );
}

#[test]
fn execute_git_commit_rejects_amend_without_head_commit() {
    let temp = tempdir().expect("tempdir");
    let dir = temp.path();
    git(dir, &["init"]);

    let error = execute_git_commit_structured(
        dir,
        GitCommitParams {
            message: "cannot exist".to_string(),
            trailers: vec![],
            amend: true,
        },
    )
    .expect_err("unborn HEAD must not be amendable");

    assert!(error.contains("cannot amend because HEAD has no commit"));
}
