# chaos-machine

Fresh machine observations for ChaOS on Linux, macOS, and FreeBSD.

## In the harness

Read `chaos://machine` to see:

- **Machine:** model, form factor, display attachment, and session context.
- **Power:** current power source and individual battery levels and states.
- **Thermals:** available CPU temperatures and OS thermal warnings.
- **Storage:** available space for the workspace, ChaOS state, and temporary files.
- **Warnings and recovery:** configured resource warnings and recovery progress.

The terminal top bar uses the same observations. Each resource read collects a
fresh snapshot; it is not a live event feed.

Unavailable information is reported as unknown, not as a healthy reading.
SSH does not mean headless, a missing battery does not mean desktop, and an
unavailable temperature does not mean cool. Containers and jails do not report
host hardware observations as their own.

Warnings can ask the agent to checkpoint and notify you. They do not
automatically save work, stop commands, or change machine settings. See
[machine warnings and recovery](../../../man/chaos-mcp.7.md#opt-in-recovery-wake)
and [the terminal top bar](../../../man/chaos-appearance.7.md#top-bar).

## In Rust

```rust
use chaos_machine::{StorageRole, StorageTarget, inspect_machine, inspect_storage};

let machine = inspect_machine();
println!("{:?}", machine.power.source);

let storage = inspect_storage(&[StorageTarget {
    role: StorageRole::Workspace,
    path: std::env::current_dir().expect("workspace directory"),
}]);
```

`inspect_machine()` returns the machine profile, power, and thermal observations.
`inspect_storage()` checks only the paths you supply, including the nearest
existing parent for a future output. Paths must be absolute. Paths on the same
filesystem share a capacity reading; unrelated disks are not scanned.

Both functions perform synchronous I/O. Call them from a blocking worker in an
async application. Observations describe the caller's machine and namespace,
not a remote execution target, and can change immediately after collection.
Available space is not a reservation or a guarantee that a write will succeed.

## Explicit indexing diagnostics

`inspect_indexing(workspace)` adds read-only indexing diagnostics on macOS and
Linux. On macOS, it resolves an existing absolute workspace's symlinks and queries
the actual mount point with `mdutil -s`, never all volumes or an assumed `/`.
A separate `ps` query lists visible Spotlight/indexer executable basenames,
PIDs, and CPU estimates. Exact executable names are matched, not command-line
substrings.

On Linux, a bounded `ps` query selects known Tracker/LocalSearch, KDE Baloo, and
`updatedb` processes. Because Linux task names can truncate to 15 bytes, candidates
are confirmed against full executable basenames through `/proc/PID/exe`.
Inaccessible candidates return an explicit failure, not an empty or partial list;
processes that exit during observation are skipped. Only recognized, visible
processes are covered; renamed tasks, hidden PIDs, and other indexers may be missed.
No user-session D-Bus connection, service control, or configuration changes are used.
There is no universal Linux indexing switch: `spotlight` remains `unsupported`
and `volume` is null on Linux. An empty process list does not mean indexing is
disabled.

The kernel includes this snapshot in the top-level `indexing` field of explicit
`chaos://machine` reads. Top-bar updates and automatic warning/recovery checks
do not collect it. No background enforcement or automatic mitigation is added.

`spotlight` and `processes` each carry a `status`: `available`, `unavailable`,
`permission_denied`, `timed_out`, or `unsupported`. Only available probes have a
`value`: Spotlight is `enabled`/`disabled`; processes is an array, possibly empty.
Missing/failed probes are not reported as disabled or as no indexers running.
FreeBSD returns `unsupported` without spawning these commands.

All commands use absolute executable paths, the C locale, no shell, no sudo,
bounded output, and a one-second deadline each. Timed-out children are killed
and reaped; the kernel's existing observation deadline also bounds the resource
wait. Filesystem resolution remains best-effort synchronous I/O.

Process observations cover the visible host, **not just this workspace**, and
do not establish which volume a process is indexing. `cpu_percent` is
`ps`'s decaying average over up to a minute on macOS and a process-lifetime average
on Linux, not an instantaneous sample; values may exceed 100.
Process names alone do not prove useful indexing work or pressure.
No settings, launchd jobs, indexes, model packs, or SIP protections are changed.

## Availability

| Observation | Linux | macOS | FreeBSD |
|---|---|---|---|
| Model and form factor | Yes | Yes | Yes |
| Power and batteries | Yes | Yes | Yes |
| Display attachment | Yes | Yes | With accessible DRM devices |
| CPU temperatures | With supported sensors | Unavailable | With supported sensors |
| OS thermal warnings | Unavailable | Yes | With supported thermal zones |
| Task-path storage | Yes | Yes | Yes |

All readings depend on hardware support and access permissions. Batteries stay
separate; the API does not invent a combined percentage or average temperatures.

FreeBSD also exposes optional lid-state and video-activity diagnostics under
`machine.profile.freebsd_acpi`. These are driver-reported hints, not proof of
physical lid position or display attachment. They do not control brightness,
power, or execution.
