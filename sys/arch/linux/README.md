# alcatraz-linux

This crate is responsible for producing:

- a `alcatraz-linux` standalone executable for Linux that is bundled with the Node.js version of the Codex CLI
- a lib crate that exposes the business logic of the executable as `run_main()` so that
  - the `chaos-fork` CLI can check if its arg0 is `alcatraz-linux` and, if so, execute as if it were `alcatraz-linux`
  - this should also be true of the `codex` multitool CLI

**Current Behavior**
- Legacy `SandboxPolicy` / `sandbox_mode` configs remain supported.
- Landlock is the Linux filesystem sandbox pipeline.
- The helper applies `PR_SET_NO_NEW_PRIVS` and a seccomp network filter
  in-process.
- Protected writable-root subpath carveouts such as `.git`, resolved `gitdir:`,
  and `.codex` are not currently enforced by the pure-Rust Linux backend.
- Split filesystem policies that need direct runtime carveouts are rejected
  instead of being silently approximated.
- In managed proxy mode, the helper fails closed unless loopback proxy
  environment variables are present.
- In managed proxy mode, seccomp blocks new AF_UNIX/socketpair creation for the
  user command and Landlock limits outbound TCP connections to the configured
  proxy ports.

**Notes**
- The CLI surface still uses legacy names like `codex debug landlock`.
