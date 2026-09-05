//! Built-in registry, ordered within each side rather than by visibility priority.

use chaos_kern::PersistenceStatus;
use chaos_sysinfo::PowerInfo;
use tokio::sync::watch;

use super::BarWidget;

pub(super) mod architecture;
pub(super) mod battery;
pub(super) mod clock;
pub(super) mod container;
pub(super) mod hostname;
pub(super) mod multiplexer;
pub(super) mod os;
pub(super) mod persistence;
pub(super) mod sandbox;
pub(super) mod storage;

pub(super) fn initial_widgets(
    name: String,
    power: watch::Receiver<PowerInfo>,
    persistence: watch::Receiver<PersistenceStatus>,
) -> Vec<BarWidget> {
    vec![
        hostname::new(name),
        storage::new(persistence.clone()),
        self::persistence::new(persistence),
        battery::new(power),
        clock::new(),
    ]
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
