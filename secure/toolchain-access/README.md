# Toolchain access

At session startup, the kernel checks the root of a trusted Git project for
`mise.toml`. This crate uses mise's installed-tool inventory without loading
project configuration or running hooks. It does not install or activate tools.

The MVP recognizes Cargo's `build.target-dir` in `.cargo/config` or
`.cargo/config.toml`, and mbx's `cache_dir` in `.mbx.toml`. An output directory
must already exist, and the repo must declare an exact version that mise reports
installed. System tools and externally linked installs do not qualify. This
does not grant access to all of `~/.cargo` or to mise's installation tree.

If an output directory needs extra write access, the kernel asks at startup,
subject to the session's approval policy. Explicit sandbox restrictions still
take precedence. The grant is scoped to the original project and cannot be
added after work starts. To pick up a changed config or a newly created output
directory, start a fresh session.
