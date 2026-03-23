# codex-linux-sandbox

This crate is responsible for producing:

- a `codex-linux-sandbox` standalone executable for Linux that is bundled with the Node.js version of the Codex CLI
- a lib crate that exposes the business logic of the executable as `run_main()` so that
  - the `codex-exec` CLI can check if its arg0 is `codex-linux-sandbox` and, if so, execute as if it were `codex-linux-sandbox`
  - this should also be true of the `codex` multitool CLI

On Linux, the bubblewrap pipeline uses the vendored bubblewrap path compiled
into this binary.

**Current Behavior**
- Legacy `SandboxPolicy` / `sandbox_mode` configs remain supported.
- Landlock is the sole filesystem sandbox pipeline on Linux.
- The helper applies `PR_SET_NO_NEW_PRIVS` and a seccomp network filter
  in-process.
- Protected subpaths under writable roots (for example `.git`, resolved
  `gitdir:`, and `.codex`) are enforced via Landlock rules.
- In managed proxy mode, the helper sets up an internal TCP->UDS->TCP routing
  bridge so tool traffic reaches only configured proxy endpoints.
- In managed proxy mode, after the bridge is live, seccomp blocks new
  AF_UNIX/socketpair creation for the user command.

**Notes**
- The CLI surface still uses legacy names like `codex debug landlock`.
