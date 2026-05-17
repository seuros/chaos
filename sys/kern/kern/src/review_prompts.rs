use chaos_ipc::protocol::ReviewRequest;
use chaos_ipc::protocol::ReviewTarget;
use chaos_scm::merge_base_with_head;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedReviewRequest {
    pub target: ReviewTarget,
    pub prompt: String,
    pub user_facing_hint: String,
    pub reviewer: Option<String>,
    /// Merge-base SHA pre-computed for `BaseBranch` targets; reused for diff fetch.
    pub merge_base_sha: Option<String>,
}

const UNCOMMITTED_PROMPT: &str = "Review the current code changes (staged, unstaged, and untracked files) and provide prioritized findings.";

const BASE_BRANCH_PROMPT_BACKUP: &str = "Review the code changes against the base branch '{branch}'. Start by finding the merge diff between the current branch and {branch}'s upstream e.g. (`git merge-base HEAD \"$(git rev-parse --abbrev-ref \"{branch}@{upstream}\")\"`), then run `git diff` against that SHA to see what changes we would merge into the {branch} branch. Provide prioritized, actionable findings.";
const BASE_BRANCH_PROMPT: &str = "Review the code changes against the base branch '{baseBranch}'. The merge base commit for this comparison is {mergeBaseSha}. Run `git diff {mergeBaseSha}` to inspect the changes relative to {baseBranch}. Provide prioritized, actionable findings.";

const COMMIT_PROMPT_WITH_TITLE: &str = "Review the code changes introduced by commit {sha} (\"{title}\"). Provide prioritized, actionable findings.";
const COMMIT_PROMPT: &str =
    "Review the code changes introduced by commit {sha}. Provide prioritized, actionable findings.";

pub fn resolve_review_request(
    request: ReviewRequest,
    cwd: &Path,
) -> anyhow::Result<ResolvedReviewRequest> {
    let target = request.target;
    let merge_base_sha = if let ReviewTarget::BaseBranch { branch } = &target {
        merge_base_with_head(cwd, branch).ok().flatten()
    } else {
        None
    };
    let prompt = review_prompt_with_merge_base(&target, cwd, merge_base_sha.as_deref())?;
    let user_facing_hint = request
        .user_facing_hint
        .unwrap_or_else(|| user_facing_hint(&target));

    Ok(ResolvedReviewRequest {
        target,
        prompt,
        user_facing_hint,
        reviewer: request.reviewer,
        merge_base_sha,
    })
}

pub fn review_prompt(target: &ReviewTarget, cwd: &Path) -> anyhow::Result<String> {
    review_prompt_with_merge_base(target, cwd, None)
}

fn review_prompt_with_merge_base(
    target: &ReviewTarget,
    cwd: &Path,
    merge_base_sha: Option<&str>,
) -> anyhow::Result<String> {
    match target {
        ReviewTarget::UncommittedChanges => Ok(UNCOMMITTED_PROMPT.to_string()),
        ReviewTarget::BaseBranch { branch } => {
            let commit = merge_base_sha
                .map(|s| Ok(Some(s.to_string())))
                .unwrap_or_else(|| merge_base_with_head(cwd, branch))?;
            if let Some(commit) = commit {
                Ok(BASE_BRANCH_PROMPT
                    .replace("{baseBranch}", branch)
                    .replace("{mergeBaseSha}", &commit))
            } else {
                Ok(BASE_BRANCH_PROMPT_BACKUP.replace("{branch}", branch))
            }
        }
        ReviewTarget::Commit { sha, title } => {
            if let Some(title) = title {
                Ok(COMMIT_PROMPT_WITH_TITLE
                    .replace("{sha}", sha)
                    .replace("{title}", title))
            } else {
                Ok(COMMIT_PROMPT.replace("{sha}", sha))
            }
        }
        ReviewTarget::Custom { instructions } => {
            let prompt = instructions.trim();
            if prompt.is_empty() {
                anyhow::bail!("Review prompt cannot be empty");
            }
            Ok(prompt.to_string())
        }
    }
}

/// Fetch the diff for a review target. Returns `None` for `Custom` targets or on error.
pub async fn fetch_review_diff(
    target: &ReviewTarget,
    cwd: &std::path::PathBuf,
    merge_base_sha: Option<&str>,
) -> Option<String> {
    match target {
        ReviewTarget::UncommittedChanges => {
            let cwd = cwd.clone();
            tokio::task::spawn_blocking(move || chaos_git::diff(&cwd, None, None).ok())
                .await
                .ok()
                .flatten()
        }
        ReviewTarget::BaseBranch { .. } => {
            // `git diff {mergeBaseSha}` includes committed, staged, and unstaged
            // changes relative to the merge base — identical scope to the prompt.
            let base = merge_base_sha?.to_string();
            let output = tokio::process::Command::new("git")
                .args([
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--end-of-options",
                    &base,
                ])
                .current_dir(cwd)
                .output()
                .await
                .ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        ReviewTarget::Commit { sha, .. } => {
            let parent = format!("{sha}^");
            let output = tokio::process::Command::new("git")
                .args([
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--end-of-options",
                    &parent,
                    sha,
                ])
                .current_dir(cwd)
                .output()
                .await
                .ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        ReviewTarget::Custom { .. } => None,
    }
}

pub fn user_facing_hint(target: &ReviewTarget) -> String {
    match target {
        ReviewTarget::UncommittedChanges => "current changes".to_string(),
        ReviewTarget::BaseBranch { branch } => format!("changes against '{branch}'"),
        ReviewTarget::Commit { sha, title } => {
            let short_sha: String = sha.chars().take(7).collect();
            if let Some(title) = title {
                format!("commit {short_sha}: {title}")
            } else {
                format!("commit {short_sha}")
            }
        }
        ReviewTarget::Custom { instructions } => instructions.trim().to_string(),
    }
}

impl From<ResolvedReviewRequest> for ReviewRequest {
    fn from(resolved: ResolvedReviewRequest) -> Self {
        ReviewRequest {
            target: resolved.target,
            user_facing_hint: Some(resolved.user_facing_hint),
            reviewer: resolved.reviewer,
        }
    }
}

#[cfg(test)]
mod tests {
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
}
