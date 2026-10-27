# alcatraz

Compile-time facade for the host operating system's sandbox backend.

A compiled Chaos binary runs on one target OS, so runtime state carries one
required `alcatraz_exe` and callers use one API. Cargo target dependencies
select exactly one implementation:

- Linux: Landlock and seccomp
- macOS: Seatbelt through `/usr/bin/sandbox-exec`
- FreeBSD: `procctl` hardening and experimental `mac_gulag` ABI 1 for
  network/process restrictions. Filesystem confinement awaits the jail/nullfs
  supervisor; restricted filesystem requests currently fail closed.
  Generic commands do not enter Capsicum capability mode.

This facade is the only package that generates the helper executable. Every
supported target therefore produces exactly one binary named `alcatraz`, backed
by that target's selected implementation.

The facade also exports the selected helper entry point, command preparation
function, and shared request/result types. There is no runtime backend registry,
missing-backend state, or set of per-OS executable options.
