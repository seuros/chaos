# alcatraz-freebsd

FreeBSD command helper using `procctl` hardening and the experimental
**mac_gulag** policy: **Glorious Unified Labor And Governance**.

The cross-platform helper executable and Cargo facade remain named `alcatraz`.
The userspace jail/process manager is **Warden** (`src/warden*.rs`).
The deliberately distinctive **GULAG** name remains kernel-side; Warden is
not a rename of the driver, its label namespace, or its ABI.
The FreeBSD-specific kernel module is `kmod/mac_gulag.c`, its output is
`mac_gulag.ko`, its MAC label element is `gulag`, and its read-only status
sysctls are `security.mac.gulag.{abi_version,enabled}`.

## Safety status

**Experimental, not production-ready. Development-driver and combined read-only live gates passed on
16-CURRENT `3e80d8b33ac6`; broader validation remains incomplete.**
Earlier pathname-policy prototypes caused NULL-label and filesystem-locking panics.

The sole current contract is **ABI 1**. This unreleased interface was renumbered
from development revision 2; there is no compatibility or migration path.
Earlier live results below predate that renumbering. Rebuild the module and
helper together; a helper expecting ABI 1 rejects a still-loaded revision-2
module. Do not reload GULAG repeatedly to switch builds: follow the load-cycle
limit below and arrange a matching module after reboot.

The selected design combines **both** FreeBSD mechanisms:

- **mac_gulag ABI 1:** per-credential network/process restrictions. No vnode
  hooks, path rules, reverse-path resolution, or filesystem enforcement.
  It builds against stock kernel headers; no FreeBSD API patch is required.
- **Warden:** userspace jail/nullfs management. Its planner, mount journal,
  descriptor-bound jail ownership/attachment, non-root child setup, minimal
  devices, read-only host view and command cancellation/cleanup are implemented.
  Native jail, private transport, helper and launcher tests passed without
  loading GULAG.
  The public execution path requires ABI 1; only test-only calls omit MAC
  adoption. A private root-launcher transport is now wired into harness startup
  and helper spawning. A production-launcher/read-only confinement probe passed with
   the real development module. An isolated interactive-console smoke also passed.
   **Broader UI validation, writable workspace layouts and sandboxed PTY
   transport remain incomplete or unsupported.**
  A planner result alone is not enforcement.

Without a root-created Warden capability, the direct helper still rejects
**every restricted filesystem request**, even if an ABI-1 module is available.
With that capability, all-read-only requests go to Warden; unsupported requests
fail there without a local-exec fallback. An environment value alone is not
authority: the owned AF_LOCAL channel must have root bootstrap credentials.

For unrestricted filesystem access with network disabled, the helper requires
the ABI-1 module, adopts `gulag/restrict!v=1!net=deny`, and verifies readback.
Other versions and all `w=`/`r=` rules are rejected, not silently ignored.
If the module is absent, disabled, or incompatible, execution stops.
Unrestricted filesystem **and** network requests need only procctl hardening.
Hardening failures also stop execution. Managed/proxy-only networking and
restricted reads remain unsupported.

This is not Capsicum capability mode. Inherited stdio descriptors are
intentional capabilities; extra inherited descriptors are closed before exec.
The earlier cache-only API draft is retained for discussion, not required by
the chosen design, and has not been applied to `/usr/src`.

## Offline checks (no module loading)

From the workspace root:

```sh
cargo test --locked -p alcatraz-freebsd
make -C sys/arch/freebsd/kmod SYSDIR=/usr/src/sys
```

The default test suite includes the production NULL-safe label accessor,
ABI-1 parser, planner, component-by-component directory pinning and a structural
check that no filesystem hooks are registered. The retired pathname adapter and
its fake-API test are removed.
Kernel-dependent tests in `tests/live.rs` are ignored unless
explicitly requested. Building does not install, load, or enable the module at
boot. Match kernel source/build configuration to the target VM kernel.

**Load-cycle limit:** this kernel reports `security.mac.max_slots=4`.
The MAC framework deliberately never reuses allocated label slots, even after
successful policy unload. Four staged development-module loads exhausted the slots on the
test host; a fifth load returned `ENOMEM`. A machine reboot restores the slot
budget; restarting the harness does not. Run remaining live tests under one
module load, not one load per case. Do not patch out this framework safeguard.

Warden's native tests are ignored by default. They create temporary jails
and mounts and require root plus an existing, exactly hide-only devfs ruleset 1.
Run a freshly built **library test binary** explicitly as root with
`native_ --ignored --nocapture --test-threads=1`; do not run Cargo as root.
These tests cover read-only mount boundaries, minimal devices, identity drop,
failed exec, background descendants, cancellation, busy cleanup and recovery.
They do **not** validate GULAG runtime behavior or a rebuilt full harness UI.
Transport tests also require the freshly built helper and
`warden_harness_probe` example paths in `CHAOS_FREEBSD_TEST_HELPER` and
`CHAOS_FREEBSD_TEST_HARNESS_PROBE`, respectively.

The separate, explicitly ignored `tests/live.rs` gates cover real MAC label
adoption, malformed labels, socket restrictions and relabel refusal.
The initial production-launcher smoke exercised non-root bootstrap capture,
the real helper, read-only jail setup, MAC adoption, output/exit status and
cleanup. The expanded `live_warden_readonly_with_production_launcher` test
additionally checks exact EROFS/EPERM denials, minimal devices and fork/exec;
it passed after reboot alongside the standalone driver gates and all 15 native
test entries under one module load. It requires root,
`CHAOS_FREEBSD_TEST_USER` (an existing non-root account), and both binary-path
variables above. Writable overlays remain blocked until their confinement
and cleanup gates are implemented.

The rebuilt main executable's `chaos sandbox` path is also live-tested through
the production launcher. Its explicit helper-only FD inheritance fixes a
previous EBADF bootstrap failure. The ignored
`live_main_sandbox_cli_with_production_launcher` regression checks confinement,
output, exit 37 and SIGTERM status, using `CHAOS_FREEBSD_TEST_MAIN` in addition
to `CHAOS_FREEBSD_TEST_HELPER` and `CHAOS_FREEBSD_TEST_USER`. Separate main-CLI
negative checks confirmed that missing Warden and unsupported workspace-write
requests never execute the target. This local CLI test does not exercise the
interactive session's oneshot/unified-exec callers.

Those actual tool paths now have a separate live fixture:
[`freebsd_warden`](../../kern/kern/tests/freebsd_warden/main.rs).
It passed through the production launcher with the real driver: oneshot
confinement, separate output, exit/signal handling, readiness-driven cancellation
and recovery; managed `exec_command` dispatch, confinement, exit/signal handling
and PTY refusal. It uses a loopback mock provider and an owned in-process journal
under a temporary `CHAOS_HOME`, not the user's journal or a remote model.
Normal test invocation skips this explicitly privileged fixture. That fixture
does not cover UI rendering or writable workspace overlays.

The separate ignored [interactive-console gate](../../../bin/chaos/tests/pty_console/warden.rs) passed
through the real production launcher: shell-group activation, jailed non-root
read-only tool execution, tool-result delivery, assistant rendering, resize
redraw and clean terminal restoration/exit. It uses a test-owned PTY, isolated
HOME/journal and loopback mock provider. No test jails/mounts remained and no
new kernel diagnostics appeared. This does not enable sandboxed-command PTYs,
writable overlays, or constitute broader/manual UI validation.

A subsequent writable-overlay experiment passed file operations but emitted a
kernel WITNESS lock-order reversal during teardown. It is not enabled or part
of the regular native test batch; see
[the quarantined review reproducer](src/warden_mount/tests.rs).

## Explicit development launch (not production deployment)

After separate ABI-1 validation, an operator can start a freshly rebuilt
harness through the existing helper:

```sh
sudo /absolute/path/to/alcatraz --warden-launch --user YOUR_USER -- /absolute/path/to/chaos
```

The launcher refuses UID 0 targets and missing/incompatible GULAG. It resolves
the user/groups through NSS, drops the harness identity, and keeps a private
socketpair and owning process descriptor. No public socket or service is
installed. Root's environment is not forwarded; normal user HOME-based config
and dotenv startup still apply.

Only sandbox helpers deliberately inherit the channel. Commands receive their
stdio capabilities, not the control channel. Requests are bounded, carry no UID,
use separate reply/cancellation channels, and run serially in this initial
implementation. Exit status/signals are preserved after cleanup. PTY, proxy-only,
restricted-read and writable-overlay requests fail closed.
