//! Fresh machine observations, not notification or execution policy.
//!
//! Chassis, display attachment, session context, and current power source are
//! independent facts. A headless laptop running on external power is valid.
//! Storage is inspected only for caller-supplied paths, never every disk.
//!
//! All probes run in the caller's host/mount namespace. They are best-effort,
//! synchronous I/O: use a blocking worker in async applications. Snapshots are
//! not atomic or cached; collect again before making a resource decision.

mod power;
mod profile;
mod storage;
mod thermal;

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
mod sysctl;

// Compile the portable platform readers/parsers on other hosts in tests too.
#[cfg(any(target_os = "freebsd", test))]
#[cfg_attr(not(target_os = "freebsd"), allow(dead_code))]
mod freebsd;
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod macos;

#[cfg(target_os = "freebsd")]
use freebsd as platform;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
compile_error!("chaos-machine supports Linux, macOS, and FreeBSD");

pub use power::{BatteryInfo, BatteryKind, BatteryState, PowerInfo, PowerSource};
pub use profile::{
    DisplayState, ExecutionEnvironment, FormFactor, FormFactorEvidence, MachineProfile,
    SessionContext,
};
pub use storage::{
    Filesystem, FilesystemBacking, FilesystemId, ResolvedStorageTarget, StorageFailure,
    StorageRole, StorageSnapshot, StorageTarget, inspect_storage,
};
pub use thermal::{CpuTemperature, TemperatureKind, TemperatureSource, ThermalInfo, ThermalState};

use serde::Serialize;
use std::time::SystemTime;

/// Observations collected starting at `observed_at`, not a health verdict.
#[derive(Debug, Clone, Serialize)]
pub struct MachineSnapshot {
    pub observed_at: SystemTime,
    pub os: &'static str,
    pub arch: &'static str,
    pub profile: MachineProfile,
    pub power: PowerInfo,
    pub thermal: ThermalInfo,
}

/// Inspect the current machine, power source, and thermals without scanning disks.
///
/// Known containers/jails do not inherit host chassis, display, battery, or thermal
/// observations from exposed host interfaces. Those facts remain unknown.
pub fn inspect_machine() -> MachineSnapshot {
    let observed_at = SystemTime::now();
    let (mut profile, power) = platform::detect();
    profile.session = SessionContext::local();
    let thermal = thermal::inspect(profile.execution_environment);
    MachineSnapshot {
        observed_at,
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        profile,
        power,
        thermal,
    }
}
