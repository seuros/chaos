# chaos-scm

Helpers for interacting with git, including patch application and branch
operations.

```rust,no_run
use std::path::Path;

use chaos_scm::{apply_git_patch, ApplyGitRequest};

let repo = Path::new("/path/to/repo");

// Apply a patch (omitted here) to the repository.
let request = ApplyGitRequest {
    cwd: repo.to_path_buf(),
    diff: String::from("...diff contents..."),
    revert: false,
    preflight: false,
};
let result = apply_git_patch(&request)?;
```
