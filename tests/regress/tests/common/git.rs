//! Git fixtures for tests that drive a real `git(1)` against scratch
//! repositories.
//!
//! Several environment knobs (`HOME`, `XDG_CONFIG_HOME`, hook globals)
//! influence what `git` does, so the suites that touch them serialize
//! through a shared mutex exposed by [`env_lock`]. The repo helpers
//! configure `core.autocrlf=false` and a deterministic identity so
//! commits are reproducible across hosts.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use std::sync::OnceLock;

use tempfile::TempDir;
use tempfile::tempdir;

/// Process-wide mutex for tests that mutate git-visible environment
/// state. Tests that only touch isolated tempdirs can skip it.
pub fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Run `git` in `repo` with the given args; panic if it exits non-zero.
pub fn run_git<I, S>(repo: &Path, args: I)
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

/// Run `git` in `repo`, capture stdout, and trim trailing whitespace.
pub fn git_stdout(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git command");
    assert!(output.status.success(), "git command failed: {args:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Initialize a fresh repo in a tempdir with a deterministic identity
/// and `core.autocrlf=false` so commits and patches reproduce on every
/// host. The returned [`TempDir`] owns the on-disk lifetime.
pub fn init_repo_with_identity() -> TempDir {
    let dir = tempdir().expect("tempdir");
    init_repo_at(dir.path());
    dir
}

/// Initialize a repo at `path` with the same identity and config as
/// [`init_repo_with_identity`]. Used when callers need to control where
/// the repository lives (for example, alongside a bare remote).
pub fn init_repo_at(path: &Path) {
    run_git(path, ["init", "--initial-branch=main"]);
    run_git(path, ["config", "core.autocrlf", "false"]);
    run_git(path, ["config", "user.name", "Chaos"]);
    run_git(path, ["config", "user.email", "chaos@example.com"]);
}

/// Stage every change under `repo` and commit with `message`.
pub fn commit_all(repo: &Path, message: &str) {
    run_git(repo, ["add", "-A"]);
    run_git(repo, ["commit", "-m", message]);
}

/// Read a file as UTF-8 with CRLF line endings collapsed to LF so
/// patch round-trip assertions don't depend on the host's line-ending
/// conventions.
pub fn read_normalized(path: &Path) -> String {
    std::fs::read_to_string(path)
        .expect("read file")
        .replace("\r\n", "\n")
}
