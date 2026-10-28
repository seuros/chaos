//! Exact byte reserves shared by disk warnings and recovery.

use crate::config::MachineWarningsConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskThresholds {
    pub(crate) warning_bytes: u128,
    // An impossible recovery target must not suppress the warning itself.
    pub(crate) recovery_bytes: Result<u128, &'static str>,
}

impl DiskThresholds {
    pub(crate) fn new(config: &MachineWarningsConfig, total_bytes: u64) -> Self {
        let total = u128::from(total_bytes);
        let cap = u128::from(config.disk_free_bytes_cap);
        let warning_scaled = total * u128::from(config.disk_free_percent);
        let recovery_percent =
            u128::from(config.disk_free_percent) + u128::from(config.recovery_disk_margin_percent);
        let percentage_target = (total * recovery_percent).div_ceil(100).min(cap);
        let byte_target =
            warning_scaled.div_ceil(100).min(cap) + u128::from(config.recovery_disk_margin_bytes);
        let recovery_bytes = percentage_target.max(byte_target);
        Self {
            // Round down for inclusive warnings, up for minimum recovery headroom.
            warning_bytes: (warning_scaled / 100).min(cap),
            recovery_bytes: if recovery_percent > 100 || recovery_bytes > total {
                Err("recovery target exceeds filesystem capacity")
            } else {
                Ok(recovery_bytes)
            },
        }
    }
}

#[cfg(test)]
mod tests;
