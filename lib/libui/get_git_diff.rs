//! Utility to compute the current Git diff for the working directory.
//!
//! Uses `chaos-git` (pure-Rust, gix-based) for all git operations — no
//! subprocess spawning. Returns the diff for tracked changes as well as
//! full-content diffs for untracked files. When the current directory is
//! not inside a Git repository, returns `Ok((false, String::new()))`.

use std::io;
use std::path::Path;

/// Return value of [`get_git_diff`].
///
/// * `bool` – Whether the current working directory is inside a Git repo.
/// * `String` – The concatenated diff (may be empty).
pub async fn get_git_diff() -> io::Result<(bool, String)> {
    // All chaos-git ops are sync (gix) — run on a blocking thread.
    tokio::task::spawn_blocking(get_git_diff_blocking)
        .await
        .map_err(|e| io::Error::other(format!("git diff task panicked: {e}")))?
}

fn get_git_diff_blocking() -> io::Result<(bool, String)> {
    let cwd = std::env::current_dir()?;

    // Check if we are inside a Git repository. Only treat NotARepo as
    // "not inside a git repo" — surface real operational errors.
    match chaos_git::repo_info(&cwd) {
        Ok(_) => {}
        Err(chaos_git::GitError::NotARepo(_)) => return Ok((false, String::new())),
        Err(e) => return Err(io::Error::other(format!("git repo check failed: {e}"))),
    }

    // Tracked diff (staged + unstaged worktree changes). On empty repos
    // with no HEAD, diff returns an empty string rather than erroring.
    let tracked_diff = match chaos_git::diff(&cwd, None, None) {
        Ok(d) => d,
        Err(chaos_git::GitError::RefNotFound(_)) => String::new(),
        Err(e) => return Err(io::Error::other(format!("git diff failed: {e}"))),
    };

    // Untracked files — generate full-content diffs in pure Rust.
    let status = match chaos_git::status(&cwd) {
        Ok(s) => s,
        Err(chaos_git::GitError::RefNotFound(_)) => chaos_git::StatusInfo {
            staged: Vec::new(),
            unstaged: Vec::new(),
            untracked: Vec::new(),
        },
        Err(e) => return Err(io::Error::other(format!("git status failed: {e}"))),
    };

    let mut untracked_diff = String::new();
    for file_status in &status.untracked {
        let path = Path::new(&file_status.path);
        let full_path = cwd.join(path);

        // Skip symlinks — match git behavior of diffing link targets
        // separately rather than dereferencing through them.
        if full_path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            continue;
        }

        match std::fs::read(&full_path) {
            Ok(bytes) => {
                // Detect binary content (NUL byte in first 8KB, same
                // heuristic git uses). Emit a marker instead of lossy text.
                let probe = &bytes[..bytes.len().min(8192)];
                if probe.contains(&0) {
                    untracked_diff.push_str(&format!(
                        "diff --git a/{p} b/{p}\nBinary files /dev/null and b/{p} differ\n",
                        p = file_status.path,
                    ));
                } else {
                    let content = String::from_utf8_lossy(&bytes);
                    untracked_diff
                        .push_str(&format!("--- /dev/null\n+++ b/{}\n", file_status.path,));
                    for line in content.lines() {
                        untracked_diff.push('+');
                        untracked_diff.push_str(line);
                        untracked_diff.push('\n');
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                // File vanished between status and read — skip.
            }
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                // Unreadable file — skip rather than failing the whole diff.
            }
            Err(err) => {
                return Err(io::Error::other(format!(
                    "failed to read untracked file `{}`: {err}",
                    file_status.path,
                )));
            }
        }
    }

    Ok((true, format!("{tracked_diff}{untracked_diff}")))
}
