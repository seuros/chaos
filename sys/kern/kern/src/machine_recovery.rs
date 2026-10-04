//! Session-local hysteresis and opt-in recovery. Absence of a warning is not recovery.
use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use chaos_machine::{BatteryKind, PowerSource, ThermalState};
use serde::Serialize;
use tokio::time::Instant;

use crate::config::MachineWarningsConfig;
use crate::machine_status::{MachineStatus, ObservationRequest};
use crate::machine_warnings::MachineWarning;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_GAP: Duration = Duration::from_secs(60);

mod lifecycle;
use lifecycle::{Observation, RecoveryObservationEvent, Wait};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Phase {
    #[default]
    Healthy,
    Warning,
    Stabilizing,
    Recovered,
    Unavailable,
    Blocked,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RecoveryStatus {
    pub phase: Phase,
    pub generation: u64,
    pub stable_seconds: u64,
    pub required_seconds: u64,
    pub wait_id: Option<String>,
    pub outstanding: Vec<MachineWarning>,
    pub interruptions_last_hour: Vec<Interruption>,
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Interruption {
    pub cause: &'static str,
    pub count: usize,
}

#[derive(Default)]
pub(crate) struct Recovery {
    pub probe: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub owner_revision: u64,
    pub sample_revision: u64,
    pub context: Option<ObservationRequest>,
    observation: Observation,
    pub generation: u64,
    wait: Wait,
    interruptions: VecDeque<(Instant, Vec<&'static str>)>,
}

impl Recovery {
    pub fn phase(&self) -> Phase {
        self.observation.phase()
    }

    pub fn wait_id(&self) -> Option<&str> {
        self.wait.id()
    }

    pub fn requested_turn(&self) -> Option<&str> {
        self.wait.requested_turn()
    }

    pub fn parked(&self) -> bool {
        self.wait.parked()
    }

    pub fn park_wait(&mut self, turn: &str) -> Option<String> {
        self.wait.park(turn)
    }

    pub fn unresolved(&self) -> bool {
        !self.observation.data().outstanding.is_empty() && self.phase() != Phase::Recovered
    }

    pub fn cancel_wait(&mut self) -> Option<String> {
        self.owner_revision = self.owner_revision.wrapping_add(1);
        self.wait.cancel()
    }

    pub fn request_wait(&mut self, turn_id: &str) -> Result<String, String> {
        if self.owner_revision != self.sample_revision {
            return Err(
                "Operator input superseded this model response; recovery wait was not registered."
                    .into(),
            );
        }
        if let Some(id) = self.wait_id() {
            return Ok(id.to_owned());
        }
        if !self.unresolved() {
            return Err("No unresolved machine warning is being monitored.".into());
        }
        let id = format!("machine-recovery:{}", uuid::Uuid::new_v4());
        self.wait.request(id.clone(), turn_id.into());
        Ok(id)
    }

    pub fn begin_sample(&mut self) {
        self.sample_revision = self.owner_revision;
    }

    pub fn observe(
        &mut self,
        observation: &Result<MachineStatus, String>,
        config: &MachineWarningsConfig,
        now: Instant,
        wall: SystemTime,
    ) {
        self.interruptions
            .retain(|(at, _)| now.duration_since(*at) < Duration::from_secs(3600));
        let gap = self
            .observation
            .data()
            .last_sample
            .is_some_and(|(last, last_wall)| {
                now.duration_since(last) > MAX_GAP
                    || wall
                        .duration_since(last_wall)
                        .map_or(true, |elapsed| elapsed > MAX_GAP)
            });
        self.observation.data_mut().last_sample = Some((now, wall));
        if gap {
            self.observation.reset_stability();
        }
        self.observation.data_mut().blocked_reason = None;
        let Ok(status) = observation else {
            if !self.observation.data().outstanding.is_empty() {
                self.observation
                    .observe(RecoveryObservationEvent::Unavailable);
            }
            return;
        };
        if !status.warnings.is_empty() {
            if self.observation.data().outstanding.is_empty()
                || self.observation.data().recovered_episode
            {
                self.generation += 1;
                self.observation.data_mut().recovered_episode = false;
                self.observation.data_mut().outstanding.clear();
                self.interruptions.push_back((now, Vec::new()));
                // Bounded even for pathological warning flapping.
                while self.interruptions.len() > 256 {
                    self.interruptions.pop_front();
                }
            }
            for warning in &status.warnings {
                if !self
                    .observation
                    .data()
                    .outstanding
                    .iter()
                    .any(|old| same_condition(old, warning))
                {
                    self.observation
                        .data_mut()
                        .outstanding
                        .push(warning.clone());
                }
                if let Some((_, causes)) = self.interruptions.back_mut()
                    && !causes.contains(&cause(warning))
                {
                    causes.push(cause(warning));
                }
            }
            self.observation.observe(RecoveryObservationEvent::Warn);
            return;
        }
        if self.observation.data().outstanding.is_empty() {
            self.observation.observe(RecoveryObservationEvent::Healthy);
            return;
        }
        let evidence = self
            .observation
            .data()
            .outstanding
            .iter()
            .try_fold(true, |healthy, warning| {
                recovered(warning, status, config).map(|next| healthy && next)
            });
        match evidence {
            Err(reason) => {
                let event = if matches!(
                    reason,
                    "recovery target exceeds filesystem capacity"
                        | "recovery target exceeds battery capacity"
                ) {
                    RecoveryObservationEvent::Blocked
                } else {
                    RecoveryObservationEvent::Unavailable
                };
                self.observation.observe(event);
                self.observation.data_mut().blocked_reason = Some(reason.into());
            }
            Ok(false) => {
                self.observation.observe(RecoveryObservationEvent::Warn);
            }
            Ok(true) => {
                let since = self.observation.healthy_since().unwrap_or(now);
                self.observation.observe(
                    if now.duration_since(since).as_secs() >= config.recovery_stable_seconds {
                        RecoveryObservationEvent::Stable(since)
                    } else {
                        RecoveryObservationEvent::Headroom(since)
                    },
                );
                self.observation.data_mut().recovered_episode |= self.phase() == Phase::Recovered;
            }
        }
    }

    pub fn status(&self, config: &MachineWarningsConfig, now: Instant) -> RecoveryStatus {
        let interruptions_last_hour = ["power", "thermal", "disk"]
            .into_iter()
            .map(|cause| Interruption {
                cause,
                count: self
                    .interruptions
                    .iter()
                    .filter(|(at, causes)| {
                        now.duration_since(*at) < Duration::from_secs(3600)
                            && causes.contains(&cause)
                    })
                    .count(),
            })
            .collect();
        RecoveryStatus {
            phase: self.phase(),
            generation: self.generation,
            stable_seconds: self
                .observation
                .healthy_since()
                .zip(self.observation.data().last_sample)
                .map_or(0, |(since, (sample, _))| {
                    sample.duration_since(since).as_secs()
                }),
            required_seconds: config.recovery_stable_seconds,
            wait_id: self.wait_id().map(str::to_owned),
            outstanding: self.observation.data().outstanding.clone(),
            interruptions_last_hour,
            blocked_reason: self.observation.data().blocked_reason.clone(),
        }
    }

    /// Fixed vocabulary only: OS names/paths must never become instructions.
    pub fn instruction(&self, config: &MachineWarningsConfig, now: Instant) -> Option<String> {
        if self.observation.data().outstanding.is_empty() {
            return None;
        }
        let status = self.status(config, now);
        let state = match self.phase() {
            Phase::Recovered => {
                "Recovery confirmed by stable observations. Reassess before resuming; recovery does not prove the previous workload is sustainable. You may reduce workload or stop rather than retry."
            }
            Phase::Stabilizing => {
                "Recovery is being observed but is not yet confirmed. Keep interruption-sensitive/heavy work paused."
            }
            Phase::Unavailable => {
                "Recovery measurements unavailable; missing observations are not safety clearance."
            }
            Phase::Blocked => {
                "Recovery target exceeds resource capacity; operator configuration or resource changes are required."
            }
            _ => {
                "Machine warning remains unresolved; keep interruption-sensitive/heavy work paused."
            }
        };
        Some(format!(
            "Machine recovery (harness host): {state}\n🩺 Kernel health checks and notifications are automatic. Required stable observation: {} seconds. Recent warning episodes (last hour): power {}, thermal {}, disk {}. Repeated thermal interruptions warrant reducing workload or stopping, not blindly restarting. Expected heat below configured/OS warning thresholds is not itself a warning. Avoid repeating unchanged notices. To opt into one recovery wake, checkpoint essential work and stop/check your heavy background processes, then call wait_for_machine_recovery alone if available. Waiting never stops existing processes automatically.",
            status.required_seconds,
            status.interruptions_last_hour[0].count,
            status.interruptions_last_hour[1].count,
            status.interruptions_last_hour[2].count,
        ))
    }
}

fn cause(warning: &MachineWarning) -> &'static str {
    match warning {
        MachineWarning::LowBattery { .. } => "power",
        MachineWarning::LowDiskSpace { .. } => "disk",
        MachineWarning::Thermal { .. } | MachineWarning::CpuTemperature { .. } => "thermal",
    }
}

fn same_condition(a: &MachineWarning, b: &MachineWarning) -> bool {
    match (a, b) {
        (
            MachineWarning::LowBattery {
                name: a,
                battery_kind: ak,
                ..
            },
            MachineWarning::LowBattery {
                name: b,
                battery_kind: bk,
                ..
            },
        ) => a == b && ak == bk,
        (
            MachineWarning::LowDiskSpace { filesystem: a, .. },
            MachineWarning::LowDiskSpace { filesystem: b, .. },
        ) => a == b,
        (MachineWarning::Thermal { .. }, MachineWarning::Thermal { .. }) => true,
        (
            MachineWarning::CpuTemperature {
                sensor: a,
                temperature_kind: ak,
                ..
            },
            MachineWarning::CpuTemperature {
                sensor: b,
                temperature_kind: bk,
                ..
            },
        ) => a == b && ak == bk,
        _ => false,
    }
}

fn recovered(
    warning: &MachineWarning,
    status: &MachineStatus,
    config: &MachineWarningsConfig,
) -> Result<bool, &'static str> {
    match warning {
        MachineWarning::LowBattery {
            name, battery_kind, ..
        } => {
            let power = &status.machine.power;
            if power.external_power == Some(true) && power.source == PowerSource::External {
                return Ok(true);
            }
            let supplying_kind = match power.source {
                PowerSource::Battery => BatteryKind::System,
                PowerSource::Ups => BatteryKind::Ups,
                _ => return Err("power source is not confirmed"),
            };
            if supplying_kind != *battery_kind || power.external_power == Some(true) {
                return Err("triggering battery supply is not confirmed");
            }
            let charge = power
                .batteries
                .iter()
                .flatten()
                .find(|battery| {
                    battery.name == *name
                        && battery.kind == *battery_kind
                        && battery.present != Some(false)
                })
                .and_then(|battery| battery.charge_percent)
                .filter(|charge| *charge <= 100)
                .ok_or("triggering battery charge unavailable")?;
            let target = u16::from(config.battery_percent)
                + u16::from(config.recovery_battery_margin_percent.max(1));
            if target > 100 {
                return Err("recovery target exceeds battery capacity");
            }
            Ok(u16::from(charge) >= target)
        }
        MachineWarning::Thermal { .. } => match status.machine.thermal.state {
            ThermalState::Normal => Ok(true),
            ThermalState::Unknown => Err("OS thermal state unavailable"),
            _ => Ok(false),
        },
        MachineWarning::CpuTemperature {
            sensor,
            temperature_kind,
            threshold,
            ..
        } => {
            let reading = status
                .machine
                .thermal
                .cpu_temperatures
                .iter()
                .find(|s| s.id == *sensor && s.kind == *temperature_kind)
                .and_then(|s| s.celsius)
                .filter(|v| v.is_finite())
                .ok_or("triggering temperature sensor unavailable")?;
            Ok(reading <= threshold - config.recovery_temperature_margin_celsius)
        }
        MachineWarning::LowDiskSpace {
            filesystem,
            targets,
            ..
        } => {
            let fs = status
                .storage
                .filesystems
                .iter()
                .find(|fs| fs.id == *filesystem)
                .filter(|fs| fs.available_percent().is_some())
                .ok_or("triggering filesystem unavailable")?;
            if targets
                .iter()
                .any(|target| !fs.targets.iter().any(|entry| entry.target == *target))
            {
                return Err("triggering storage target unavailable or remounted");
            }
            let total = u128::from(fs.total_bytes);
            let available = u128::from(fs.available_bytes);
            let percent = u128::from(config.disk_free_percent)
                + u128::from(config.recovery_disk_margin_percent);
            let bytes = (total * u128::from(config.disk_free_percent)).div_ceil(100)
                + u128::from(config.recovery_disk_margin_bytes);
            if percent > 100 || bytes > total {
                return Err("recovery target exceeds filesystem capacity");
            }
            Ok(available * 100 >= total * percent && available >= bytes)
        }
    }
}

#[cfg(test)]
mod tests;
