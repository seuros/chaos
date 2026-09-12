//! Built-in registry, ordered within each side rather than by visibility priority.

use chaos_kern::PersistenceStatus;
use chaos_kern::machine_status::MachineStatus;
use tokio::sync::watch;

use super::machine::Source;
use super::{BarWidget, Content, Side};

pub(super) mod architecture;
pub(super) mod battery;
pub(super) mod clock;
pub(super) mod container;
pub(super) mod disk;
pub(super) mod hostname;
pub(super) mod multiplexer;
pub(super) mod os;
pub(super) mod persistence;
pub(super) mod profile;
pub(super) mod sandbox;
pub(super) mod storage;
pub(super) mod thermal;
pub(super) mod version;

pub(super) fn initial_widgets(
    name: String,
    machine: Source,
    persistence: watch::Receiver<PersistenceStatus>,
) -> Vec<BarWidget> {
    vec![
        version::new(),
        hostname::new(name),
        profile::new(machine.clone()),
        storage::new(persistence.clone()),
        self::persistence::new(persistence),
        disk::new(machine.clone()),
        thermal::new(machine.clone()),
        battery::new(machine),
        clock::new(),
    ]
}

/// Shared presentation adapter; readings/thresholds come from the kernel, not UI probes.
fn observed(
    id: &'static str,
    priority: u8,
    source: Source,
    present: fn(&MachineStatus) -> Content,
) -> BarWidget {
    BarWidget::watched(id, Side::Right, priority, source, move |snapshot| {
        snapshot.as_deref().map(present).unwrap_or_default()
    })
    .with_warning_priority(250)
}

/// Static environment widgets in their original left-side order.
///
/// The runtime collects the system snapshot in a background worker and appends
/// these after the already-available hostname. No detection happens in rendering.
pub(super) fn environment_widgets(info: &chaos_sysinfo::SystemInfo) -> Vec<BarWidget> {
    vec![
        os::new(&info.os, &info.os_distro),
        architecture::new(info.arch.clone()),
        sandbox::new(info.sandbox_type),
        container::new(info.in_container, &info.container_type),
        multiplexer::new(info.multiplexer.as_ref()),
    ]
}
