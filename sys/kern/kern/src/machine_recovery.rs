//! Session-local hysteresis and opt-in recovery. Absence of a warning is not recovery.
use std::collections::VecDeque;
use std::time::{Duration, SystemTime};

use chaos_machine::{PowerSource, ThermalState};
use serde::Serialize;
use tokio::time::Instant;

use crate::config::MachineWarningsConfig;
use crate::machine_status::{MachineStatus, ObservationRequest};
use crate::machine_warnings::MachineWarning;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_GAP: Duration = Duration::from_secs(60);

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
    pub phase: Phase,
    pub generation: u64,
    pub wait_id: Option<String>,
    /// A tool request is only activated after the entire sample's tool batch ends.
    pub requested_turn: Option<String>,
    pub parked: bool,
    outstanding: Vec<MachineWarning>,
    healthy_since: Option<Instant>,
    last_sample: Option<(Instant, SystemTime)>,
    interruptions: VecDeque<(Instant, Vec<&'static str>)>,
    blocked_reason: Option<String>,
    recovered_episode: bool,
}

impl Recovery {
    pub fn unresolved(&self) -> bool {
        !self.outstanding.is_empty() && self.phase != Phase::Recovered
    }

    pub fn cancel_wait(&mut self) -> Option<String> {
        self.owner_revision = self.owner_revision.wrapping_add(1);
        self.requested_turn = None;
        self.parked = false;
        self.wait_id.take()
    }

    pub fn request_wait(&mut self, turn_id: &str) -> Result<String, String> {
        if self.owner_revision != self.sample_revision {
            return Err(
                "Operator input superseded this model response; recovery wait was not registered."
                    .into(),
            );
        }
        if let Some(id) = &self.wait_id {
            return Ok(id.clone());
        }
        if !self.unresolved() {
            return Err("No unresolved machine warning is being monitored.".into());
        }
        let id = format!("machine-recovery:{}", uuid::Uuid::new_v4());
        self.wait_id = Some(id.clone());
        self.requested_turn = Some(turn_id.into());
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
        let gap = self.last_sample.is_some_and(|(last, last_wall)| {
            now.duration_since(last) > MAX_GAP
                || wall
                    .duration_since(last_wall)
                    .map_or(true, |elapsed| elapsed > MAX_GAP)
        });
        self.last_sample = Some((now, wall));
        if gap {
            self.healthy_since = None;
        }
        self.blocked_reason = None;
        let Ok(status) = observation else {
            self.healthy_since = None;
            if !self.outstanding.is_empty() {
                self.phase = Phase::Unavailable;
            }
            return;
        };
        if !status.warnings.is_empty() {
            if self.outstanding.is_empty() || self.recovered_episode {
                self.generation += 1;
                self.recovered_episode = false;
                self.outstanding.clear();
                self.interruptions.push_back((now, Vec::new()));
                // Bounded even for pathological warning flapping.
                while self.interruptions.len() > 256 {
                    self.interruptions.pop_front();
                }
            }
            for warning in &status.warnings {
                if !self
                    .outstanding
                    .iter()
                    .any(|old| same_condition(old, warning))
                {
                    self.outstanding.push(warning.clone());
                }
                if let Some((_, causes)) = self.interruptions.back_mut()
                    && !causes.contains(&cause(warning))
                {
                    causes.push(cause(warning));
                }
            }
            self.healthy_since = None;
            self.phase = Phase::Warning;
            return;
        }
        if self.outstanding.is_empty() {
            self.phase = Phase::Healthy;
            return;
        }
        let evidence = self.outstanding.iter().try_fold(true, |healthy, warning| {
            recovered(warning, status, config).map(|next| healthy && next)
        });
        match evidence {
            Err(reason) => {
                self.phase = if reason == "recovery target exceeds filesystem capacity" {
                    Phase::Blocked
                } else {
                    Phase::Unavailable
                };
                self.blocked_reason = Some(reason.into());
                self.healthy_since = None;
            }
            Ok(false) => {
                self.phase = Phase::Warning;
                self.healthy_since = None;
            }
            Ok(true) => {
                let since = self.healthy_since.get_or_insert(now);
                self.phase =
                    if now.duration_since(*since).as_secs() >= config.recovery_stable_seconds {
                        Phase::Recovered
                    } else {
                        Phase::Stabilizing
                    };
                self.recovered_episode |= self.phase == Phase::Recovered;
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
            phase: self.phase,
            generation: self.generation,
            stable_seconds: self
                .healthy_since
                .zip(self.last_sample)
                .map_or(0, |(since, (sample, _))| {
                    sample.duration_since(since).as_secs()
                }),
            required_seconds: config.recovery_stable_seconds,
            wait_id: self.wait_id.clone(),
            outstanding: self.outstanding.clone(),
            interruptions_last_hour,
            blocked_reason: self.blocked_reason.clone(),
        }
    }

    /// Fixed vocabulary only: OS names/paths must never become instructions.
    pub fn instruction(&self, config: &MachineWarningsConfig, now: Instant) -> Option<String> {
        if self.outstanding.is_empty() {
            return None;
        }
        let status = self.status(config, now);
        let state = match self.phase {
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
                "Recovery target exceeds filesystem capacity; operator configuration or storage changes are required."
            }
            _ => {
                "Machine warning remains unresolved; keep interruption-sensitive/heavy work paused."
            }
        };
        Some(format!(
            "Machine recovery (harness host): {state}\nRequired stable observation: {} seconds. Recent warning episodes (last hour): power {}, thermal {}, disk {}. Repeated thermal interruptions warrant reducing workload or stopping, not blindly restarting. Expected heat below configured/OS warning thresholds is not itself a warning. Avoid repeating unchanged notices. To opt into one recovery wake, checkpoint essential work and stop/check your heavy background processes, then call wait_for_machine_recovery alone if available. Waiting never stops existing processes automatically.",
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
        MachineWarning::LowBattery { .. } => match status.machine.power.external_power {
            Some(true) if status.machine.power.source == PowerSource::External => Ok(true),
            Some(false) => Ok(false),
            _ => Err("external power is not confirmed"),
        },
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
