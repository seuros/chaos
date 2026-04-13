mod capture;
mod git_ops;
mod options;
mod restore;
mod snapshot;

pub use options::CreateGhostCommitOptions;
pub use options::GhostSnapshotConfig;
pub use options::GhostSnapshotReport;
pub use options::IgnoredUntrackedFile;
pub use options::LargeUntrackedDir;
pub use options::RestoreGhostCommitOptions;
pub use snapshot::capture_ghost_snapshot_report;
pub use snapshot::create_ghost_commit;
pub use snapshot::create_ghost_commit_with_report;
pub use snapshot::restore_ghost_commit;
pub use snapshot::restore_ghost_commit_with_options;
pub use snapshot::restore_to_commit;

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs::File;
    use std::io;
    use std::path::Component;
    use std::path::Path;
    use std::path::PathBuf;
    use std::process::Command;

    use assert_matches::assert_matches;
    use pretty_assertions::assert_eq;
    use walkdir::WalkDir;

    use crate::GitToolingError;
    use crate::operations::run_git_for_stdout;

    use super::capture::DEFAULT_IGNORED_DIR_NAMES;
    use super::options::DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS;
    use super::options::DEFAULT_IGNORE_LARGE_UNTRACKED_FILES;
    use super::*;

    /// Runs a git command in the test repository and asserts success.
    fn run_git_in(repo_path: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repo_path)
            .args(args)
            .status()
            .expect("git command");
        assert!(status.success(), "git command failed: {args:?}");
    }

    /// Runs a git command and returns its trimmed stdout output.
    fn run_git_stdout(repo_path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repo_path)
            .args(args)
            .output()
            .expect("git command");
        assert!(output.status.success(), "git command failed: {args:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// Initializes a repository with consistent settings for cross-platform tests.
    fn init_test_repo(repo: &Path) {
        run_git_in(repo, &["init", "--initial-branch=main"]);
        run_git_in(repo, &["config", "core.autocrlf", "false"]);
    }

    fn create_sparse_file(path: &Path, bytes: i64) -> io::Result<()> {
        let file_len =
            u64::try_from(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let file = File::create(path)?;
        file.set_len(file_len)?;
        Ok(())
    }

    #[test]
    /// Verifies a ghost commit can be created and restored end to end.
    fn create_and_restore_roundtrip() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);
        std::fs::write(repo.join("tracked.txt"), "initial\n")?;
        std::fs::write(repo.join("delete-me.txt"), "to be removed\n")?;
        run_git_in(repo, &["add", "tracked.txt", "delete-me.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ],
        );

        let preexisting_untracked = repo.join("notes.txt");
        std::fs::write(&preexisting_untracked, "notes before\n")?;

        let tracked_contents = "modified contents\n";
        std::fs::write(repo.join("tracked.txt"), tracked_contents)?;
        std::fs::remove_file(repo.join("delete-me.txt"))?;
        let new_file_contents = "hello ghost\n";
        std::fs::write(repo.join("new-file.txt"), new_file_contents)?;
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        let ignored_contents = "ignored but captured\n";
        std::fs::write(repo.join("ignored.txt"), ignored_contents)?;

        let options =
            CreateGhostCommitOptions::new(repo).force_include(vec![PathBuf::from("ignored.txt")]);
        let ghost = create_ghost_commit(&options)?;

        assert!(ghost.parent().is_some());
        let cat = run_git_for_stdout(
            repo,
            vec![
                OsString::from("show"),
                OsString::from(format!("{}:ignored.txt", ghost.id())),
            ],
            None,
        )?;
        assert_eq!(cat, ignored_contents.trim());

        std::fs::write(repo.join("tracked.txt"), "other state\n")?;
        std::fs::write(repo.join("ignored.txt"), "changed\n")?;
        std::fs::remove_file(repo.join("new-file.txt"))?;
        std::fs::write(repo.join("ephemeral.txt"), "temp data\n")?;
        std::fs::write(&preexisting_untracked, "notes after\n")?;

        restore_ghost_commit(repo, &ghost)?;

        let tracked_after = std::fs::read_to_string(repo.join("tracked.txt"))?;
        assert_eq!(tracked_after, tracked_contents);
        let ignored_after = std::fs::read_to_string(repo.join("ignored.txt"))?;
        assert_eq!(ignored_after, ignored_contents);
        let new_file_after = std::fs::read_to_string(repo.join("new-file.txt"))?;
        assert_eq!(new_file_after, new_file_contents);
        assert_eq!(repo.join("delete-me.txt").exists(), false);
        assert!(!repo.join("ephemeral.txt").exists());
        let notes_after = std::fs::read_to_string(&preexisting_untracked)?;
        assert_eq!(notes_after, "notes before\n");

        Ok(())
    }

    #[test]
    fn snapshot_ignores_large_untracked_files() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join("tracked.txt"), "contents\n")?;
        run_git_in(repo, &["add", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let big = repo.join("big.bin");
        let big_size = 2 * 1024 * 1024;
        create_sparse_file(&big, big_size)?;

        let (ghost, report) = create_ghost_commit_with_report(
            &CreateGhostCommitOptions::new(repo).ignore_large_untracked_files(1024),
        )?;
        assert!(ghost.parent().is_some());
        assert_eq!(
            report.ignored_untracked_files,
            vec![IgnoredUntrackedFile {
                path: PathBuf::from("big.bin"),
                byte_size: big_size,
            }]
        );

        let exists_in_commit = Command::new("git")
            .current_dir(repo)
            .args(["cat-file", "-e", &format!("{}:big.bin", ghost.id())])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!exists_in_commit);

        std::fs::write(repo.join("ephemeral.txt"), "temp\n")?;
        restore_ghost_commit(repo, &ghost)?;
        assert!(
            big.exists(),
            "big.bin should be preserved during undo cleanup"
        );
        assert!(!repo.join("ephemeral.txt").exists());

        Ok(())
    }

    #[test]
    fn create_snapshot_reports_large_untracked_dirs() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join("tracked.txt"), "contents\n")?;
        run_git_in(repo, &["add", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let models = repo.join("models");
        std::fs::create_dir(&models)?;
        let threshold = DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS;
        for idx in 0..(threshold + 1) {
            let file = models.join(format!("weights-{idx}.bin"));
            std::fs::write(file, "data\n")?;
        }

        let (ghost, report) =
            create_ghost_commit_with_report(&CreateGhostCommitOptions::new(repo))?;
        assert!(ghost.parent().is_some());
        assert_eq!(
            report.large_untracked_dirs,
            vec![LargeUntrackedDir {
                path: PathBuf::from("models"),
                file_count: threshold + 1,
            }]
        );

        let exists_in_commit = Command::new("git")
            .current_dir(repo)
            .args([
                "cat-file",
                "-e",
                &format!("{}:models/weights-0.bin", ghost.id()),
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!exists_in_commit);

        std::fs::write(repo.join("ephemeral.txt"), "temp\n")?;
        restore_ghost_commit(repo, &ghost)?;
        assert!(
            repo.join("models/weights-0.bin").exists(),
            "ignored untracked directories should be preserved during undo cleanup"
        );
        assert!(!repo.join("ephemeral.txt").exists());

        Ok(())
    }

    #[test]
    fn restore_preserves_large_untracked_dirs_when_threshold_disabled()
    -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join("tracked.txt"), "contents\n")?;
        run_git_in(repo, &["add", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let models = repo.join("models");
        std::fs::create_dir(&models)?;
        let threshold: i64 = 2;
        for idx in 0..(threshold + 1) {
            let file = models.join(format!("weights-{idx}.bin"));
            std::fs::write(file, "data\n")?;
        }

        let snapshot_config = GhostSnapshotConfig {
            ignore_large_untracked_files: Some(DEFAULT_IGNORE_LARGE_UNTRACKED_FILES),
            ignore_large_untracked_dirs: Some(threshold),
            disable_warnings: false,
        };
        let (ghost, _report) = create_ghost_commit_with_report(
            &CreateGhostCommitOptions::new(repo).ghost_snapshot(snapshot_config),
        )?;

        std::fs::write(repo.join("ephemeral.txt"), "temp\n")?;
        restore_ghost_commit_with_options(
            &RestoreGhostCommitOptions::new(repo).ignore_large_untracked_dirs(0),
            &ghost,
        )?;

        assert!(
            repo.join("models/weights-0.bin").exists(),
            "ignored untracked directories should be preserved during undo cleanup, even when the threshold is disabled at restore time"
        );
        assert!(!repo.join("ephemeral.txt").exists());

        Ok(())
    }

    #[test]
    fn snapshot_ignores_default_ignored_directories() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join("tracked.txt"), "contents\n")?;
        run_git_in(repo, &["add", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let node_modules = repo.join("node_modules");
        std::fs::create_dir_all(node_modules.join("@scope/package/src"))?;
        for idx in 0..50 {
            let file = node_modules.join(format!("file-{idx}.js"));
            std::fs::write(file, "console.log('ignored');\n")?;
        }
        std::fs::write(
            node_modules.join("@scope/package/src/index.js"),
            "console.log('nested ignored');\n",
        )?;

        let venv = repo.join(".venv");
        std::fs::create_dir_all(venv.join("lib/python/site-packages"))?;
        std::fs::write(
            venv.join("lib/python/site-packages/pkg.py"),
            "print('ignored')\n",
        )?;

        let (ghost, report) =
            create_ghost_commit_with_report(&CreateGhostCommitOptions::new(repo))?;
        assert!(ghost.parent().is_some());

        for file in ghost.preexisting_untracked_files() {
            let components = file.components().collect::<Vec<_>>();
            let mut has_default_ignored_component = false;
            for component in components {
                if let Component::Normal(name) = component
                    && let Some(name_str) = name.to_str()
                    && DEFAULT_IGNORED_DIR_NAMES
                        .iter()
                        .any(|ignored| ignored == &name_str)
                {
                    has_default_ignored_component = true;
                    break;
                }
            }
            assert!(
                !has_default_ignored_component,
                "unexpected default-ignored file captured: {file:?}"
            );
        }

        for dir in ghost.preexisting_untracked_dirs() {
            let components = dir.components().collect::<Vec<_>>();
            let mut has_default_ignored_component = false;
            for component in components {
                if let Component::Normal(name) = component
                    && let Some(name_str) = name.to_str()
                    && DEFAULT_IGNORED_DIR_NAMES
                        .iter()
                        .any(|ignored| ignored == &name_str)
                {
                    has_default_ignored_component = true;
                    break;
                }
            }
            assert!(
                !has_default_ignored_component,
                "unexpected default-ignored dir captured: {dir:?}"
            );
        }

        for entry in &report.large_untracked_dirs {
            let components = entry.path.components().collect::<Vec<_>>();
            let mut has_default_ignored_component = false;
            for component in components {
                if let Component::Normal(name) = component
                    && let Some(name_str) = name.to_str()
                    && DEFAULT_IGNORED_DIR_NAMES
                        .iter()
                        .any(|ignored| ignored == &name_str)
                {
                    has_default_ignored_component = true;
                    break;
                }
            }
            assert!(
                !has_default_ignored_component,
                "unexpected default-ignored dir in large_untracked_dirs: {:?}",
                entry.path
            );
        }

        Ok(())
    }

    #[test]
    fn restore_preserves_default_ignored_directories() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join("tracked.txt"), "snapshot version\n")?;
        run_git_in(repo, &["add", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let node_modules = repo.join("node_modules");
        std::fs::create_dir_all(node_modules.join("pkg"))?;
        std::fs::write(
            node_modules.join("pkg/index.js"),
            "console.log('before');\n",
        )?;

        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo))?;

        std::fs::write(repo.join("tracked.txt"), "snapshot delta\n")?;
        std::fs::write(node_modules.join("pkg/index.js"), "console.log('after');\n")?;
        std::fs::write(node_modules.join("pkg/extra.js"), "console.log('extra');\n")?;
        std::fs::write(repo.join("temp.txt"), "new file\n")?;

        restore_ghost_commit(repo, &ghost)?;

        let tracked_after = std::fs::read_to_string(repo.join("tracked.txt"))?;
        assert_eq!(tracked_after, "snapshot version\n");

        let node_modules_exists = node_modules.exists();
        assert!(node_modules_exists);

        let files_under_node_modules: Vec<_> = WalkDir::new(&node_modules)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .collect();
        assert!(!files_under_node_modules.is_empty());

        assert!(!repo.join("temp.txt").exists());

        Ok(())
    }

    #[test]
    fn create_snapshot_reports_nested_large_untracked_dirs_under_tracked_parent()
    -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        // Create a tracked src directory.
        let src = repo.join("src");
        std::fs::create_dir(&src)?;
        std::fs::write(src.join("main.rs"), "fn main() {}\n")?;
        run_git_in(repo, &["add", "src/main.rs"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        // Create a large untracked tree nested under the tracked src directory.
        let generated = src.join("generated").join("cache");
        std::fs::create_dir_all(&generated)?;
        let threshold = DEFAULT_IGNORE_LARGE_UNTRACKED_DIRS;
        for idx in 0..(threshold + 1) {
            let file = generated.join(format!("file-{idx}.bin"));
            std::fs::write(file, "data\n")?;
        }

        let (ghost, report) =
            create_ghost_commit_with_report(&CreateGhostCommitOptions::new(repo))?;
        assert_eq!(report.large_untracked_dirs.len(), 1);
        let entry = &report.large_untracked_dirs[0];
        assert_ne!(entry.path, PathBuf::from("src"));
        assert!(
            entry.path.starts_with(Path::new("src/generated")),
            "unexpected path for large untracked directory: {}",
            entry.path.display()
        );
        assert_eq!(entry.file_count, threshold + 1);

        let exists_in_commit = Command::new("git")
            .current_dir(repo)
            .args([
                "cat-file",
                "-e",
                &format!("{}:src/generated/cache/file-0.bin", ghost.id()),
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        assert!(!exists_in_commit);

        Ok(())
    }

    #[test]
    /// Ensures ghost commits succeed in repositories without an existing HEAD.
    fn create_snapshot_without_existing_head() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        let tracked_contents = "first contents\n";
        std::fs::write(repo.join("tracked.txt"), tracked_contents)?;
        let ignored_contents = "ignored but captured\n";
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        std::fs::write(repo.join("ignored.txt"), ignored_contents)?;

        let options =
            CreateGhostCommitOptions::new(repo).force_include(vec![PathBuf::from("ignored.txt")]);
        let ghost = create_ghost_commit(&options)?;

        assert!(ghost.parent().is_none());

        let message = run_git_stdout(repo, &["log", "-1", "--format=%s", ghost.id()]);
        assert_eq!(message, "chaos snapshot");

        let ignored = run_git_stdout(repo, &["show", &format!("{}:ignored.txt", ghost.id())]);
        assert_eq!(ignored, ignored_contents.trim());

        Ok(())
    }

    #[test]
    /// Confirms custom messages are used when creating ghost commits.
    fn create_ghost_commit_uses_custom_message() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join("tracked.txt"), "contents\n")?;
        run_git_in(repo, &["add", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let message = "custom message";
        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo).message(message))?;
        let commit_message = run_git_stdout(repo, &["log", "-1", "--format=%s", ghost.id()]);
        assert_eq!(commit_message, message);

        Ok(())
    }

    #[test]
    /// Rejects force-included paths that escape the repository.
    fn create_ghost_commit_rejects_force_include_parent_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path();
        init_test_repo(repo);
        let options = CreateGhostCommitOptions::new(repo)
            .force_include(vec![PathBuf::from("../outside.txt")]);
        let err = create_ghost_commit(&options).unwrap_err();
        assert_matches!(err, GitToolingError::PathEscapesRepository { .. });
    }

    #[test]
    /// Restoring a ghost commit from a non-git directory fails.
    fn restore_requires_git_repository() {
        let temp = tempfile::tempdir().expect("tempdir");
        let err = restore_to_commit(temp.path(), "deadbeef").unwrap_err();
        assert_matches!(err, GitToolingError::NotAGitRepository { .. });
    }

    #[test]
    /// Restoring from a subdirectory affects only that subdirectory.
    fn restore_from_subdirectory_restores_files_relatively() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::create_dir_all(repo.join("workspace"))?;
        let workspace = repo.join("workspace");
        std::fs::write(repo.join("root.txt"), "root contents\n")?;
        std::fs::write(workspace.join("nested.txt"), "nested contents\n")?;
        run_git_in(repo, &["add", "."]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        std::fs::write(repo.join("root.txt"), "root modified\n")?;
        std::fs::write(workspace.join("nested.txt"), "nested modified\n")?;

        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(&workspace))?;

        std::fs::write(repo.join("root.txt"), "root after\n")?;
        std::fs::write(workspace.join("nested.txt"), "nested after\n")?;

        restore_ghost_commit(&workspace, &ghost)?;

        let root_after = std::fs::read_to_string(repo.join("root.txt"))?;
        assert_eq!(root_after, "root after\n");
        let nested_after = std::fs::read_to_string(workspace.join("nested.txt"))?;
        assert_eq!(nested_after, "nested modified\n");
        assert!(!workspace.join("chaos").exists());

        Ok(())
    }

    #[test]
    /// Restoring from a subdirectory preserves ignored files in parent folders.
    fn restore_from_subdirectory_preserves_parent_vscode() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        let workspace = repo.join("chaos");
        std::fs::create_dir_all(&workspace)?;
        std::fs::write(repo.join(".gitignore"), ".vscode/\n")?;
        std::fs::write(workspace.join("tracked.txt"), "snapshot version\n")?;
        run_git_in(repo, &["add", "."]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        std::fs::write(workspace.join("tracked.txt"), "snapshot delta\n")?;
        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(&workspace))?;

        std::fs::write(workspace.join("tracked.txt"), "post-snapshot\n")?;
        let vscode = repo.join(".vscode");
        std::fs::create_dir_all(&vscode)?;
        std::fs::write(vscode.join("settings.json"), "{\n  \"after\": true\n}\n")?;

        restore_ghost_commit(&workspace, &ghost)?;

        let tracked_after = std::fs::read_to_string(workspace.join("tracked.txt"))?;
        assert_eq!(tracked_after, "snapshot delta\n");
        assert!(vscode.join("settings.json").exists());
        let settings_after = std::fs::read_to_string(vscode.join("settings.json"))?;
        assert_eq!(settings_after, "{\n  \"after\": true\n}\n");

        Ok(())
    }

    #[test]
    /// Restoring from the repository root keeps ignored files intact.
    fn restore_preserves_ignored_files() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join(".gitignore"), ".vscode/\n")?;
        std::fs::write(repo.join("tracked.txt"), "snapshot version\n")?;
        let vscode = repo.join(".vscode");
        std::fs::create_dir_all(&vscode)?;
        std::fs::write(vscode.join("settings.json"), "{\n  \"before\": true\n}\n")?;
        run_git_in(repo, &["add", ".gitignore", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        std::fs::write(repo.join("tracked.txt"), "snapshot delta\n")?;
        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo))?;

        std::fs::write(repo.join("tracked.txt"), "post-snapshot\n")?;
        std::fs::write(vscode.join("settings.json"), "{\n  \"after\": true\n}\n")?;
        std::fs::write(repo.join("temp.txt"), "new file\n")?;

        restore_ghost_commit(repo, &ghost)?;

        let tracked_after = std::fs::read_to_string(repo.join("tracked.txt"))?;
        assert_eq!(tracked_after, "snapshot delta\n");
        assert!(vscode.join("settings.json").exists());
        let settings_after = std::fs::read_to_string(vscode.join("settings.json"))?;
        assert_eq!(settings_after, "{\n  \"after\": true\n}\n");
        assert!(!repo.join("temp.txt").exists());

        Ok(())
    }

    #[test]
    /// Restoring leaves ignored directories created after the snapshot untouched.
    fn restore_preserves_new_ignored_directory() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join(".gitignore"), ".vscode/\n")?;
        std::fs::write(repo.join("tracked.txt"), "snapshot version\n")?;
        run_git_in(repo, &["add", ".gitignore", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo))?;

        let vscode = repo.join(".vscode");
        std::fs::create_dir_all(&vscode)?;
        std::fs::write(vscode.join("settings.json"), "{\n  \"after\": true\n}\n")?;

        restore_ghost_commit(repo, &ghost)?;

        assert!(vscode.exists());
        let settings_after = std::fs::read_to_string(vscode.join("settings.json"))?;
        assert_eq!(settings_after, "{\n  \"after\": true\n}\n");

        Ok(())
    }

    #[test]
    /// Restoring leaves ignored files created after the snapshot untouched.
    fn restore_preserves_new_ignored_file() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        std::fs::write(repo.join("tracked.txt"), "snapshot version\n")?;
        run_git_in(repo, &["add", ".gitignore", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo))?;

        let ignored = repo.join("ignored.txt");
        std::fs::write(&ignored, "created later\n")?;

        restore_ghost_commit(repo, &ghost)?;

        assert!(ignored.exists());
        let contents = std::fs::read_to_string(&ignored)?;
        assert_eq!(contents, "created later\n");

        Ok(())
    }

    #[test]
    /// Restoring keeps deleted ignored files deleted when they were absent before the snapshot.
    fn restore_respects_removed_ignored_file() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        std::fs::write(repo.join("tracked.txt"), "snapshot version\n")?;
        let ignored = repo.join("ignored.txt");
        std::fs::write(&ignored, "initial state\n")?;
        run_git_in(repo, &["add", ".gitignore", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo))?;

        std::fs::remove_file(&ignored)?;

        restore_ghost_commit(repo, &ghost)?;

        assert!(!ignored.exists());

        Ok(())
    }

    #[test]
    /// Restoring leaves files matched by glob ignores intact.
    fn restore_preserves_ignored_glob_matches() -> Result<(), GitToolingError> {
        let temp = tempfile::tempdir()?;
        let repo = temp.path();
        init_test_repo(repo);

        std::fs::write(repo.join(".gitignore"), "dummy-dir/*.txt\n")?;
        std::fs::write(repo.join("tracked.txt"), "snapshot version\n")?;
        run_git_in(repo, &["add", ".gitignore", "tracked.txt"]);
        run_git_in(
            repo,
            &[
                "-c",
                "user.name=Tester",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );

        let ghost = create_ghost_commit(&CreateGhostCommitOptions::new(repo))?;

        let dummy_dir = repo.join("dummy-dir");
        std::fs::create_dir_all(&dummy_dir)?;
        let file1 = dummy_dir.join("file1.txt");
        let file2 = dummy_dir.join("file2.txt");
        std::fs::write(&file1, "first\n")?;
        std::fs::write(&file2, "second\n")?;

        restore_ghost_commit(repo, &ghost)?;

        assert!(file1.exists());
        assert!(file2.exists());
        assert_eq!(std::fs::read_to_string(file1)?, "first\n");
        assert_eq!(std::fs::read_to_string(file2)?, "second\n");

        Ok(())
    }
}
