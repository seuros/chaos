# chaos-git

Pure-Rust Git tooling built on Gitoxide. It exposes repository inspection plus
two narrow mutation tools without shelling out to `git`:

- `git_add(paths)` stages explicit repository-relative files and deletions.
- `git_commit(message)` creates an unsigned commit from the staged index.

Mutation tools reject directories, ignored new files, unresolved conflicts,
submodules, sparse indexes, and in-progress repository operations. Commits use
the configured Git identity and do not run Git hooks. Existing unchanged
gitlinks are preserved when committing unrelated staged files.
