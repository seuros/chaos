# chaos-git

Pure-Rust Git tooling built on Gitoxide. It exposes repository inspection plus
three narrow mutation tools without shelling out to `git`:

- `git_add(paths)` stages explicit repository-relative files and deletions.
- `git_commit(message, amend=false)` creates an unsigned commit from the staged
  index. With `amend=true`, it replaces `HEAD`, preserves its parents and author,
  and includes any staged changes; no staged changes are required for a
  message-only amend.
- `git_branch(operation, name, ...)` creates or deletes a local branch.

Mutation tools reject directories, ignored new files, unresolved conflicts,
submodules, sparse indexes, and in-progress repository operations. Commits use
the configured Git identity and do not run Git hooks. Existing unchanged
gitlinks are preserved when committing unrelated staged files.
