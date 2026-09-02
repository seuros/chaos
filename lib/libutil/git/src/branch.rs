use std::path::Path;

use gix::bstr::ByteSlice;
use gix::refs::FullName;
use gix::refs::transaction::PreviousValue;
use serde::Serialize;

use crate::error::GitError;
use crate::ext::GitResultExt;
use crate::open_repo;

#[derive(Debug, Clone, Serialize)]
pub struct BranchMutationResult {
    pub operation: &'static str,
    pub branch: String,
    pub oid: String,
}

fn full_branch_name(name: &str) -> Result<FullName, GitError> {
    if name.trim() != name || name.is_empty() {
        return Err(GitError::InvalidInput(
            "branch name must be non-empty and contain no surrounding whitespace".to_string(),
        ));
    }
    if name.starts_with("refs/") {
        return Err(GitError::InvalidInput(
            "branch name must be a short local name without a refs/heads/ prefix".to_string(),
        ));
    }

    let full = format!("refs/heads/{name}");
    gix::validate::reference::branch_name(full.as_bytes().as_bstr()).map_err(|error| {
        GitError::InvalidInput(format!("invalid branch name {name:?}: {error}"))
    })?;
    full.try_into()
        .map_err(|error| GitError::InvalidInput(format!("invalid branch name {name:?}: {error}")))
}

pub fn create(
    cwd: &Path,
    name: &str,
    start_point: Option<&str>,
) -> Result<BranchMutationResult, GitError> {
    let repo = open_repo(cwd)?;
    let full_name = full_branch_name(name)?;
    let start_point = start_point.unwrap_or("HEAD");
    let target = repo
        .rev_parse_single(start_point)
        .map_err(|error| GitError::RefNotFound(format!("{start_point}: {error}")))?
        .object()
        .git_op()?
        .peel_to_commit()
        .git_op()?
        .id()
        .detach();

    repo.reference(
        full_name,
        target,
        PreviousValue::MustNotExist,
        format!("branch: Created from {start_point}"),
    )
    .git_op()?;

    Ok(BranchMutationResult {
        operation: "create",
        branch: name.to_string(),
        oid: target.to_string(),
    })
}

pub fn delete(cwd: &Path, name: &str, force: bool) -> Result<BranchMutationResult, GitError> {
    let mut repo = open_repo(cwd)?;
    let full_name = full_branch_name(name)?;
    let branch_id = repo
        .find_reference(&full_name)
        .map_err(|error| GitError::RefNotFound(format!("{name}: {error}")))?
        .id()
        .detach();

    if !force {
        let head_id = repo.head_commit().git_op()?.id().detach();
        let merge_base = repo.merge_base(branch_id, head_id).git_op()?.detach();
        if merge_base != branch_id {
            return Err(GitError::Operation(format!(
                "branch {name:?} is not fully merged into HEAD; pass force=true to delete it"
            )));
        }
    }

    repo.delete_local_branches([full_name]).git_op()?;

    Ok(BranchMutationResult {
        operation: "delete",
        branch: name.to_string(),
        oid: branch_id.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "Test User"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        fs::write(dir.join("file.txt"), "initial\n").expect("write file");
        git(dir, &["add", "file.txt"]);
        git(dir, &["commit", "-m", "initial"]);
    }

    #[test]
    fn create_does_not_checkout_and_delete_removes_merged_branch() {
        let temp = tempdir().expect("tempdir");
        init_repo(temp.path());

        let created = create(temp.path(), "topic", None).expect("create branch");
        assert_eq!(created.operation, "create");
        assert_eq!(
            crate::branches(temp.path())
                .expect("branches")
                .current
                .as_deref(),
            Some("main")
        );
        assert!(
            crate::branches(temp.path())
                .expect("branches")
                .local
                .contains(&"topic".to_string())
        );

        let deleted = delete(temp.path(), "topic", false).expect("delete merged branch");
        assert_eq!(deleted.oid, created.oid);
        assert!(
            !crate::branches(temp.path())
                .expect("branches")
                .local
                .contains(&"topic".to_string())
        );
    }

    #[test]
    fn delete_rejects_unmerged_branch_without_force() {
        let temp = tempdir().expect("tempdir");
        init_repo(temp.path());
        git(temp.path(), &["switch", "-c", "topic"]);
        fs::write(temp.path().join("topic.txt"), "topic\n").expect("write topic");
        git(temp.path(), &["add", "topic.txt"]);
        git(temp.path(), &["commit", "-m", "topic"]);
        git(temp.path(), &["switch", "main"]);

        let error = delete(temp.path(), "topic", false).expect_err("reject unmerged");
        assert!(error.to_string().contains("not fully merged"));
        delete(temp.path(), "topic", true).expect("force delete");
    }

    #[test]
    fn delete_rejects_branch_checked_out_in_linked_worktree() {
        let temp = tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("worktree");
        fs::create_dir(&repo).expect("create repo");
        init_repo(&repo);
        create(&repo, "topic", None).expect("create topic");
        git(
            &repo,
            &[
                "worktree",
                "add",
                worktree.to_str().expect("utf8 worktree"),
                "topic",
            ],
        );

        let error = delete(&repo, "topic", true).expect_err("reject checked out branch");
        assert!(error.to_string().contains("checked out"));
    }

    #[test]
    fn rejects_full_reference_names() {
        let temp = tempdir().expect("tempdir");
        init_repo(temp.path());
        let error = create(temp.path(), "refs/heads/topic", None).expect_err("reject full name");
        assert!(error.to_string().contains("short local name"));
    }
}
