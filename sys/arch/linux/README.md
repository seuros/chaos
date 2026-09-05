# alcatraz-linux

This crate provides the Linux implementation selected by the `alcatraz` facade:

- sandbox command preparation and the `run_main()` helper entry point
  used by the facade's single `alcatraz` executable
- library APIs so that
  - the `chaos-fork` CLI can check if its arg0 is `alcatraz` and, if so, execute as the platform helper
  - this should also be true of the `chaos` multitool CLI

**Current Behavior**
- Legacy `SandboxPolicy` / `sandbox_mode` configs remain supported.
- Landlock is the Linux filesystem sandbox pipeline.
- The helper applies `PR_SET_NO_NEW_PRIVS` and a seccomp network filter
  in-process.
- Protected writable-root subpath carveouts such as `.git`, resolved `gitdir:`,
  and `.chaos` are not currently enforced by the pure-Rust Linux backend.
- Split filesystem policies that need direct runtime carveouts are rejected
  instead of being silently approximated.
- In managed proxy mode, the helper fails closed unless loopback proxy
  environment variables are present.
- In managed proxy mode, seccomp blocks new AF_UNIX/socketpair creation for the
  user command and Landlock limits outbound TCP connections to the configured
  proxy ports.

**Notes**
- The CLI surface still uses legacy names like `chaos debug landlock`.
