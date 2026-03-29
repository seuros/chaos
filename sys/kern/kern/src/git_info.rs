use std::collections::BTreeMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use crate::util::resolve_path;
use chaos_ipc::api::GitSha;
use chaos_ipc::protocol::GitInfo;
use futures::future::join_all;
use serde::Deserialize;
use serde::Serialize;
use tokio::process::Command;
use tokio::time::Duration as TokioDuration;
use tokio::time::timeout;

/// Return `true` if the project folder specified by the `Config` is inside a
/// Git repository.
///
/// The check walks up the directory hierarchy looking for a `.git` file or
/// directory (note `.git` can be a file that contains a `gitdir` entry). This
/// approach does **not** require the `git` binary or the `git2` crate and is
/// therefore fairly lightweight.
///
/// Note that this does **not** detect *work-trees* created with
/// `git worktree add` where the checkout lives outside the main repository
/// directory. If you need Codex to work from such a checkout simply pass the
/// `--allow-no-git-exec` CLI flag that disables the repo requirement.
pub fn get_git_repo_root(base_dir: &Path) -> Option<PathBuf> {
    chaos_git::repo_info(base_dir).ok().map(|info| info.root)
}

/// Timeout for git commands to prevent freezing on large repositories.
/// Retained for `git_diff_to_remote` which still shells out.
const GIT_COMMAND_TIMEOUT: TokioDuration = TokioDuration::from_secs(5);

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct GitDiffToRemote {
    pub sha: GitSha,
    pub diff: String,
}

/// Collect git repository information from the given working directory using
/// `chaos-git` (pure-Rust, no subprocess). Returns None if no git repository
/// is found.
pub async fn collect_git_info(cwd: &Path) -> Option<GitInfo> {
    let info = chaos_git::repo_info(cwd).ok()?;

    let mut git_info = GitInfo {
        commit_hash: None,
        branch: None,
        repository_url: None,
    };

    git_info.commit_hash = info.head_sha;
    git_info.branch = info.branch;
    git_info.repository_url = info.remote_url;

    Some(git_info)
}

/// Collect fetch remotes in a multi-root-friendly format: {"origin": "https://..."}.
pub async fn get_git_remote_urls(cwd: &Path) -> Option<BTreeMap<String, String>> {
    let remote_info = chaos_git::remotes(cwd).ok()?;
    if remote_info.remotes.is_empty() {
        None
    } else {
        Some(remote_info.remotes)
    }
}

/// Collect fetch remotes without checking whether `cwd` is in a git repo.
pub async fn get_git_remote_urls_assume_git_repo(cwd: &Path) -> Option<BTreeMap<String, String>> {
    get_git_remote_urls(cwd).await
}

/// Return the current HEAD commit hash.
pub async fn get_head_commit_hash(cwd: &Path) -> Option<String> {
    chaos_git::repo_info(cwd).ok()?.head_sha
}

pub async fn get_has_changes(cwd: &Path) -> Option<bool> {
    chaos_git::status(cwd).ok().map(|status| {
        !(status.staged.is_empty() && status.unstaged.is_empty() && status.untracked.is_empty())
    })
}

/// A minimal commit summary entry used for pickers (subject + timestamp + sha).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitLogEntry {
    pub sha: String,
    /// Unix timestamp (seconds since epoch) of the commit time (committer time).
    pub timestamp: i64,
    /// Single-line subject of the commit message.
    pub subject: String,
}

/// Return the last `limit` commits reachable from HEAD for the current branch.
/// Each entry contains the SHA, commit timestamp (seconds), and subject line.
/// Returns an empty vector if not in a git repo or on error/timeout.
pub async fn recent_commits(cwd: &Path, limit: usize) -> Vec<CommitLogEntry> {
    let entries = match chaos_git::log(cwd, Some(limit), None) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    entries
        .into_iter()
        .map(|e| CommitLogEntry {
            sha: e.sha,
            timestamp: e.timestamp,
            subject: e.subject.trim_end_matches('\n').to_string(),
        })
        .collect()
}

/// Returns the closest git sha to HEAD that is on a remote as well as the diff to that sha.
///
/// This function still shells out to `git(1)` because it relies on complex
/// branch ancestry and rev-list distance logic that `chaos-git` does not
/// replicate yet.
pub async fn git_diff_to_remote(cwd: &Path) -> Option<GitDiffToRemote> {
    get_git_repo_root(cwd)?;

    let remotes = get_git_remotes(cwd).await?;
    let branches = branch_ancestry(cwd).await?;
    let base_sha = find_closest_sha(cwd, &branches, &remotes).await?;
    let diff = diff_against_sha(cwd, &base_sha).await?;

    Some(GitDiffToRemote {
        sha: base_sha,
        diff,
    })
}

/// Run a git command with a timeout to prevent blocking on large repositories.
/// Retained for `git_diff_to_remote` and its helpers.
async fn run_git_command_with_timeout(args: &[&str], cwd: &Path) -> Option<std::process::Output> {
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true);
    let result = timeout(GIT_COMMAND_TIMEOUT, command.output()).await;

    match result {
        Ok(Ok(output)) => Some(output),
        _ => None, // Timeout or error
    }
}

async fn get_git_remotes(cwd: &Path) -> Option<Vec<String>> {
    let output = run_git_command_with_timeout(&["remote"], cwd).await?;
    if !output.status.success() {
        return None;
    }
    let mut remotes: Vec<String> = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .map(str::to_string)
        .collect();
    if let Some(pos) = remotes.iter().position(|r| r == "origin") {
        let origin = remotes.remove(pos);
        remotes.insert(0, origin);
    }
    Some(remotes)
}

/// Determine the repository's default branch name, if available.
///
/// Uses `chaos-git` to inspect remote configuration and common local defaults.
pub async fn default_branch_name(cwd: &Path) -> Option<String> {
    chaos_git::repo_info(cwd).ok()?.default_branch
}

/// Build an ancestry of branches starting at the current branch and ending at the
/// repository's default branch (if determinable).
async fn branch_ancestry(cwd: &Path) -> Option<Vec<String>> {
    // Discover current branch (ignore detached HEAD by treating it as None)
    let current_branch = run_git_command_with_timeout(&["rev-parse", "--abbrev-ref", "HEAD"], cwd)
        .await
        .and_then(|o| {
            if o.status.success() {
                String::from_utf8(o.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())
        .filter(|s| s != "HEAD");

    // Discover default branch
    let default_branch = default_branch_name(cwd).await;

    let mut ancestry: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(cb) = current_branch.clone() {
        seen.insert(cb.clone());
        ancestry.push(cb);
    }
    if let Some(db) = default_branch
        && !seen.contains(&db)
    {
        seen.insert(db.clone());
        ancestry.push(db);
    }

    // Expand candidates: include any remote branches that already contain HEAD.
    // This addresses cases where we're on a new local-only branch forked from a
    // remote branch that isn't the repository default. We prioritize remotes in
    // the order returned by get_git_remotes (origin first).
    let remotes = get_git_remotes(cwd).await.unwrap_or_default();
    for remote in remotes {
        if let Some(output) = run_git_command_with_timeout(
            &[
                "for-each-ref",
                "--format=%(refname:short)",
                "--contains=HEAD",
                &format!("refs/remotes/{remote}"),
            ],
            cwd,
        )
        .await
            && output.status.success()
            && let Ok(text) = String::from_utf8(output.stdout)
        {
            for line in text.lines() {
                let short = line.trim();
                // Expect format like: "origin/feature"; extract the branch path after "remote/"
                if let Some(stripped) = short.strip_prefix(&format!("{remote}/"))
                    && !stripped.is_empty()
                    && !seen.contains(stripped)
                {
                    seen.insert(stripped.to_string());
                    ancestry.push(stripped.to_string());
                }
            }
        }
    }

    // Ensure we return Some vector, even if empty, to allow caller logic to proceed
    Some(ancestry)
}

// Helper for a single branch: return the remote SHA if present on any remote
// and the distance (commits ahead of HEAD) for that branch. The first item is
// None if the branch is not present on any remote. Returns None if distance
// could not be computed due to git errors/timeouts.
async fn branch_remote_and_distance(
    cwd: &Path,
    branch: &str,
    remotes: &[String],
) -> Option<(Option<GitSha>, usize)> {
    // Try to find the first remote ref that exists for this branch (origin prioritized by caller).
    let mut found_remote_sha: Option<GitSha> = None;
    let mut found_remote_ref: Option<String> = None;
    for remote in remotes {
        let remote_ref = format!("refs/remotes/{remote}/{branch}");
        let Some(verify_output) =
            run_git_command_with_timeout(&["rev-parse", "--verify", "--quiet", &remote_ref], cwd)
                .await
        else {
            // Mirror previous behavior: if the verify call times out/fails at the process level,
            // treat the entire branch as unusable.
            return None;
        };
        if !verify_output.status.success() {
            continue;
        }
        let Ok(sha) = String::from_utf8(verify_output.stdout) else {
            // Mirror previous behavior and skip the entire branch on parse failure.
            return None;
        };
        found_remote_sha = Some(GitSha::new(sha.trim()));
        found_remote_ref = Some(remote_ref);
        break;
    }

    // Compute distance as the number of commits HEAD is ahead of the branch.
    // Prefer local branch name if it exists; otherwise fall back to the remote ref (if any).
    let count_output = if let Some(local_count) =
        run_git_command_with_timeout(&["rev-list", "--count", &format!("{branch}..HEAD")], cwd)
            .await
    {
        if local_count.status.success() {
            local_count
        } else if let Some(remote_ref) = &found_remote_ref {
            match run_git_command_with_timeout(
                &["rev-list", "--count", &format!("{remote_ref}..HEAD")],
                cwd,
            )
            .await
            {
                Some(remote_count) => remote_count,
                None => return None,
            }
        } else {
            return None;
        }
    } else if let Some(remote_ref) = &found_remote_ref {
        match run_git_command_with_timeout(
            &["rev-list", "--count", &format!("{remote_ref}..HEAD")],
            cwd,
        )
        .await
        {
            Some(remote_count) => remote_count,
            None => return None,
        }
    } else {
        return None;
    };

    if !count_output.status.success() {
        return None;
    }
    let Ok(distance_str) = String::from_utf8(count_output.stdout) else {
        return None;
    };
    let Ok(distance) = distance_str.trim().parse::<usize>() else {
        return None;
    };

    Some((found_remote_sha, distance))
}

// Finds the closest sha that exist on any of branches and also exists on any of the remotes.
async fn find_closest_sha(cwd: &Path, branches: &[String], remotes: &[String]) -> Option<GitSha> {
    // A sha and how many commits away from HEAD it is.
    let mut closest_sha: Option<(GitSha, usize)> = None;
    for branch in branches {
        let Some((maybe_remote_sha, distance)) =
            branch_remote_and_distance(cwd, branch, remotes).await
        else {
            continue;
        };
        let Some(remote_sha) = maybe_remote_sha else {
            // Preserve existing behavior: skip branches that are not present on a remote.
            continue;
        };
        match &closest_sha {
            None => closest_sha = Some((remote_sha, distance)),
            Some((_, best_distance)) if distance < *best_distance => {
                closest_sha = Some((remote_sha, distance));
            }
            _ => {}
        }
    }
    closest_sha.map(|(sha, _)| sha)
}

async fn diff_against_sha(cwd: &Path, sha: &GitSha) -> Option<String> {
    let output =
        run_git_command_with_timeout(&["diff", "--no-textconv", "--no-ext-diff", &sha.0], cwd)
            .await?;
    // 0 is success and no diff.
    // 1 is success but there is a diff.
    let exit_ok = output.status.code().is_some_and(|c| c == 0 || c == 1);
    if !exit_ok {
        return None;
    }
    let mut diff = String::from_utf8(output.stdout).ok()?;

    if let Some(untracked_output) =
        run_git_command_with_timeout(&["ls-files", "--others", "--exclude-standard"], cwd).await
        && untracked_output.status.success()
    {
        let untracked: Vec<String> = String::from_utf8(untracked_output.stdout)
            .ok()?
            .lines()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect();

        if !untracked.is_empty() {
            // Use platform-appropriate null device and guard paths with `--`.
            let null_device: &str = "/dev/null";
            let futures_iter = untracked.into_iter().map(|file| async move {
                let file_owned = file;
                let args_vec: Vec<&str> = vec![
                    "diff",
                    "--no-textconv",
                    "--no-ext-diff",
                    "--binary",
                    "--no-index",
                    // -- ensures that filenames that start with - are not treated as options.
                    "--",
                    null_device,
                    &file_owned,
                ];
                run_git_command_with_timeout(&args_vec, cwd).await
            });
            let results = join_all(futures_iter).await;
            for extra in results.into_iter().flatten() {
                if extra.status.code().is_some_and(|c| c == 0 || c == 1)
                    && let Ok(s) = String::from_utf8(extra.stdout)
                {
                    diff.push_str(&s);
                }
            }
        }
    }

    Some(diff)
}

/// Resolve the path that should be used for trust checks. Similar to
/// `[get_git_repo_root]`, but resolves to the root of the main
/// repository. Handles worktrees via filesystem inspection without invoking
/// the `git` executable.
pub fn resolve_root_git_project_for_trust(cwd: &Path) -> Option<PathBuf> {
    let base = if cwd.is_dir() { cwd } else { cwd.parent()? };
    let (repo_root, dot_git) = find_ancestor_git_entry(base)?;
    if dot_git.is_dir() {
        return Some(canonicalize_or_raw(repo_root));
    }

    let git_dir_s = std::fs::read_to_string(&dot_git).ok()?;
    let git_dir_rel = git_dir_s.trim().strip_prefix("gitdir:")?.trim();
    if git_dir_rel.is_empty() {
        return None;
    }

    let git_dir_path = canonicalize_or_raw(resolve_path(&repo_root, &PathBuf::from(git_dir_rel)));
    let worktrees_dir = git_dir_path.parent()?;
    if worktrees_dir.file_name() != Some(OsStr::new("worktrees")) {
        return None;
    }

    let common_dir = worktrees_dir.parent()?;
    let main_repo_root = common_dir.parent()?;
    Some(canonicalize_or_raw(main_repo_root.to_path_buf()))
}

fn find_ancestor_git_entry(base_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut dir = base_dir.to_path_buf();

    loop {
        let dot_git = dir.join(".git");
        if dot_git.exists() {
            return Some((dir, dot_git));
        }

        // Pop one component (go up one directory). `pop` returns false when
        // we have reached the filesystem root.
        if !dir.pop() {
            break;
        }
    }

    None
}

fn canonicalize_or_raw(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// Returns a list of local git branches.
/// Includes the default branch at the beginning of the list, if it exists.
pub async fn local_git_branches(cwd: &Path) -> Vec<String> {
    let info = match chaos_git::branches(cwd) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    // The chaos-git crate already sorts and puts the default branch first.
    info.local
}

/// Returns the current checked out branch name.
pub async fn current_branch_name(cwd: &Path) -> Option<String> {
    chaos_git::branches(cwd).ok()?.current
}

#[cfg(test)]
#[path = "git_info_tests.rs"]
mod tests;
