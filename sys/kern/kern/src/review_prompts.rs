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
mod tests;
