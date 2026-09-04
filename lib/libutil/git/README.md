# chaos-git

Pure-Rust Git tooling built on Gitoxide. It exposes repository inspection plus
three narrow mutation tools without shelling out to `git`:

- `git_add(paths)` stages explicit repository-relative files and deletions.
- `git_commit(message, trailers=[], amend=false)` creates an unsigned commit
  from the staged index. Trailers are structured `{token, value}` entries. With
  `amend=true`, it replaces `HEAD`, preserves its parents and author, and
  includes any staged changes; no staged changes are required for a message-only
  amend.
- `git_branch(operation, name, ...)` creates or deletes a local branch.

Mutation tools reject directories, ignored new files, unresolved conflicts,
submodules, sparse indexes, and in-progress repository operations. Commits use
the configured Git identity and do not run Git hooks. Existing unchanged
gitlinks are preserved when committing unrelated staged files.

For example:

```json
{
  "message": "Implement commit trailers",
  "trailers": [
    {
      "token": "Signed-off-by",
      "value": "Mira Tenner <mira-agent@agentmail.to>"
    },
    {
      "token": "Co-authored-by",
      "value": "Daniel Tenner <daniel@tenner.org>"
    }
  ]
}
```

The API keeps trailers separate from `message`; Git stores them as canonical
final lines in the commit message. The result reports `operation`, `sha`,
`previous_sha` for an amend, branch state, subject, normalized trailers, and
committed paths.
