# chaos-machine

Detection foundation for environment-aware decisions. **No notifications,
thresholds, execution blocks, background polling, or configuration are enabled
by this crate.** The kernel exposes fresh observations through `chaos://machine`;
its separate warning policy can instruct the model to checkpoint and notify the
operator. The terminal top bar consumes the same observations and kernel policy.

## Independent facts

- **Form factor:** laptop, desktop, server, tablet, other, or unknown, with the
  classification evidence. Missing batteries do not imply desktops.
- **Displays:** connected, none detected (headless), or unknown. A laptop can be
  headless. SSH and graphical-session hints are separate from display detection.
- **Execution environment:** container/jail, VM, none detected, or unknown.
  These are heuristics, not security attestations. Known containers/jails do not
  report exposed host chassis, displays, batteries, or thermals as their own.
- **Power:** external, system battery, UPS, or unknown. Adapter presence and
  each system/UPS battery's presence, percentage, and state are separate.
  Peripheral batteries are excluded where the OS marks them as device-scoped.
- **Thermals:** CPU temperature channels and system-wide OS thermal warnings
  are separate facts. Missing data never means cool.

`PowerInfo::on_battery_power()` is false for a dead or removed battery running
on external power. The OS can report AC power while a battery discharges
(maintenance or battery assistance); retain both facts rather than overriding
the primary source. Unknown is not false. Battery health/backup availability
and interruption-sensitive operations need separate policy, not this boolean.
Multiple batteries remain separate rather than inventing a system percentage
by taking the first, minimum, or unweighted average.

## Thermals

`MachineSnapshot::thermal` contains the strongest recognized OS thermal state
(`normal`, `warning`, `critical`, or `unknown`) and CPU-associated readings.
Each channel retains its OS-local identifier, label, source, temperature kind,
value, and driver/OS critical limit when available. Limits are observations,
not configured warning thresholds.

- Linux recognizes `coretemp`, `k10temp`, `k8temp`, and `via_cputemp` hwmon
  drivers plus explicitly CPU-named thermal zones. GPU, SSD, and unclassified
  board/SoC sensors are not substituted for CPU readings.
- AMD Tctl is marked `control`, not physical die temperature. FreeBSD's generic
  CPU sysctl does not establish the temperature kind, so it stays `unknown`.
  Compare limits only within the same channel/scale, not across unlike sensors.
- Channels are not averaged or deduplicated into an invented package value.
  A core/package can appear through more than one OS interface.
- Missing, faulted, disabled, unreadable, malformed, or implausible values are
  unknown. The broad sanity range of -100°C through 250°C is not a health
  threshold; valid zero/subzero and overheating readings are preserved.
- macOS reports OS thermal warning/pressure state, not fabricated Celsius.
  `NSProcessInfo` fair/serious pressure means warning; critical means critical.
  Its nominal value also covers unsupported hardware, so only a successful
  normal IOPM warning observation can establish `normal`.

Detection is best-effort, not proof that every CPU sensor was enumerated or that
cooling is safe. No thermal state is inferred from Celsius or CPU utilization.
Unpublished OS warnings and absent sensor drivers remain unknown. No private
macOS sensor interfaces, privileged subprocesses, or driver loading are used.

## Task-scoped storage

Pass only paths that matter: workspace, ChaOS state, temporary work, outputs,
or checkpoints. There is no enumeration of all mounted drives. Three full
archive SSDs are irrelevant unless the task actually uses them.

- Resolve each path through symlinks to its filesystem.
- For a future output, probe the nearest existing ancestor.
- Group paths sharing filesystem identity and read-only view.
- Report caller-available bytes, total bytes, available inodes where supported,
  and read-only status. No summing free space across unrelated disks.
- Preserve temporary-path roles even when `/tmp` is not RAM-backed.
- Keep probe failures separate from measured zero free space.

Paths must be absolute. Dangling symlinks, access errors, and ambiguous missing
paths containing `..` are errors, not reasons to guess a different filesystem.
Probes do not test write access, reserve space, predict output size, or guarantee
per-user quota availability. Filesystem IDs are local to the current namespace,
not stable physical-drive identifiers. Paths/mounts can change after a probe.

`backing: other` is **not a durability guarantee**. A filesystem can live on a
RAM disk or in a disposable container; temporary files can be removed at reboot
or by cleanup even on ordinary disks.

## API

`inspect_machine()` returns machine, power, and thermal observations.
`inspect_storage(&[StorageTarget])` inspects the supplied task paths.

The harness resource `chaos://machine` is discoverable through
`list_mcp_resources` and readable with `read_mcp_resource`, omitting `server`.
It returns compact JSON with `scope: "harness_host"`, `machine`, `storage`, and
`warnings`, plus `warning_instruction` when a configured threshold is reached.
Every read runs new probes on a blocking worker; it is not a cached startup
snapshot or a subscription. The kernel bounds the wait and keeps timed-out probes
from accumulating workers.

Kernel sessions also include `recovery` status: outstanding conditions, stability
progress, an optional live-session wait ID, and recent interruption counts. Failed
reads return `observation_error` alongside recovery status, not invented readings.
The kernel's `wait_for_machine_recovery` tool can park an opted-in model until
five minutes of stable recovery with headroom. This only wakes the model to
reassess; it never restarts commands. See the
[recovery policy](../../../man/chaos-mcp.7.md#opt-in-recovery-wake).

The in-session resource probes the active turn's cwd, configured ChaOS
home/cache/log directories, and the process temporary directory. The standalone
MCP server uses its configured cwd rather than guessing which child session the
caller means. Only those paths are covered: other outputs/checkpoints, custom
database URL locations, per-command temporary-directory overrides, and remote
tool hosts are not inferred. Probe failures stay separate from zero free space.

Probes are fresh, synchronous, best-effort I/O. Use a blocking worker in async
applications; timestamps mark collection start, not an atomic observation.
Inspect the execution host/namespace, not a remote target from the local host.
Async kernel/UI consumers share `chaos_kern::machine_status::ObservationRequest`:
it captures active paths/policy and returns typed `MachineStatus` observations,
with the same deadline and single-worker limit as the resource and model checks.

## Platform coverage

| | Linux | macOS | FreeBSD |
|---|---|---|---|
| Form factor | SMBIOS chassis; system-battery fallback | Recognized model identifier; system-battery fallback | System-battery fallback |
| Power | sysfs power supplies | `pmset -g batt`, bounded to two seconds | ACPI sysctls |
| Displays | DRM connector status | CoreGraphics online display list | Unknown |
| CPU temperature | Recognized hwmon/thermal-zone channels, millidegrees Celsius | Unavailable | CPU temperature/Tjmax sysctls, decikelvin |
| OS thermal state | Unknown | IOPM warnings plus `NSProcessInfo` elevated pressure | Unknown |
| Storage | `statvfs` / `statfs` | `statvfs` / `statfs` | `statvfs` / `statfs` |

An inaccessible macOS Quartz session is not headless evidence. Generic Apple
identifiers such as `Mac14,2` do not identify chassis by
themselves. Without stronger evidence, the result stays unknown. Legacy Linux
display drivers, missing sysfs/ACPI, and restricted probes can also be unknown.
VM/container detection is best-effort and not exhaustive.

`chaos-sysinfo` remains the legacy startup/static-environment snapshot API.
This crate deliberately does not derive safety facts from its lossy battery
booleans or process-wide cached disk statistics. The kernel's
[`machine_warnings` settings](../../../man/chaos-mcp.7.md#machine-warnings)
default to 5% battery, 5% available filesystem space, and OS/sensor thermal warnings.
Warnings are also injected before normal model requests, not just resource reads.
Checkpointing is an instruction to the model, not an automatic save. There is no
continuous model notification, interruption of running commands, or execution interlock.
The [terminal top bar](../../../man/chaos-appearance.7.md#top-bar) polls the shared
bounded kernel collector for display only; it does not replace fresh pre-request
or resource observations. Its local clock continues updating independently of probes.
