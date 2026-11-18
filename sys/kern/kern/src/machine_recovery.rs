//! Session-local hysteresis and opt-in recovery. Absence of a warning is not recovery.
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use deadline_machines::continuous::ContinuousClock;
use feedback_machines::{Band, Hysteresis, Thresholds};
use serde::Serialize;
use vitals_machines::probe::{BatteryKind, PowerSource, ThermalState};
use vitals_machines::{Episode, Sample};

use crate::config::MachineWarningsConfig;
use crate::machine_status::{MachineStatus, ObservationRequest};
use crate::machine_warnings::MachineWarning;
use crate::machine_warnings::disk::DiskThresholds;

pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_GAP: Duration = Duration::from_secs(60);
const HOUR: Duration = Duration::from_secs(3600);

mod identity;
mod lifecycle;
use identity::same_condition;
use lifecycle::Wait;

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

impl From<vitals_machines::Phase> for Phase {
    fn from(phase: vitals_machines::Phase) -> Self {
        use vitals_machines::Phase as Vitals;
        match phase {
            Vitals::Healthy => Self::Healthy,
            Vitals::Warning => Self::Warning,
            Vitals::Stabilizing => Self::Stabilizing,
            Vitals::Recovered => Self::Recovered,
            Vitals::Unavailable => Self::Unavailable,
            Vitals::Blocked => Self::Blocked,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RecoveryStatus {
    pub phase: Phase,
    pub generation: u64,
    /// Allocated durably with the first checkpoint of this live recovery.
    pub recovery_id: Option<i64>,
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

#[derive(Clone)]
pub(crate) struct Clock(Arc<dyn Fn() -> Duration + Send + Sync>);

impl Default for Clock {
    fn default() -> Self {
        let clock = ContinuousClock::new();
        Self(Arc::new(move || clock.now()))
    }
}

impl Clock {
    pub(crate) fn now(&self) -> Duration {
        (self.0)()
    }

    #[cfg(test)]
    pub(crate) fn from_fn(now: impl Fn() -> Duration + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }
}

struct Outstanding {
    side: Band,
    warning: MachineWarning,
}

impl Outstanding {
    fn new(warning: MachineWarning) -> Self {
        Self {
            side: alarmed(&warning),
            warning,
        }
    }
}

pub(crate) struct Recovery {
    pub probe: Arc<tokio::sync::Mutex<()>>,
    pub checkpoint_writer: Arc<tokio::sync::Mutex<()>>,
    pub owner_revision: u64,
    pub sample_revision: u64,
    pub context: Option<ObservationRequest>,
    pub clock: Clock,
    scope: u16,
    episodes: vitals_machines::Recovery,
    recovery_id: Option<i64>,
    outstanding: Vec<Outstanding>,
    blocked_reason: Option<String>,
    wait: Wait,
    interruptions: VecDeque<(Duration, Vec<&'static str>)>,
}

impl Default for Recovery {
    fn default() -> Self {
        Self::in_scope(0)
    }
}

impl Recovery {
    fn in_scope(scope: u16) -> Self {
        Self {
            probe: Arc::default(),
            checkpoint_writer: Arc::default(),
            owner_revision: 0,
            sample_revision: 0,
            context: None,
            clock: Clock::default(),
            scope,
            episodes: vitals_machines::Recovery::with_scope(scope, MAX_GAP),
            recovery_id: None,
            outstanding: Vec::new(),
            blocked_reason: None,
            wait: Wait::default(),
            interruptions: VecDeque::new(),
        }
    }

    /// A slow checkpoint must not bind to an episode of the new context.
    pub fn replace_context(&mut self, context: ObservationRequest) {
        let mut next = Self::in_scope(self.scope.wrapping_add(1));
        next.probe = self.probe.clone();
        next.checkpoint_writer = self.checkpoint_writer.clone();
        next.owner_revision = self.owner_revision;
        next.sample_revision = self.sample_revision;
        next.clock = self.clock.clone();
        next.context = Some(context);
        *self = next;
    }

    pub fn now(&self) -> Duration {
        self.clock.now()
    }

    pub fn phase(&self) -> Phase {
        self.episodes.phase().into()
    }

    pub fn generation(&self) -> u64 {
        self.episodes.episode().map_or(0, Episode::raw)
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

    /// What the outstanding warnings are about, in a fixed vocabulary.
    pub fn causes(&self) -> Vec<&'static str> {
        let mut causes: Vec<_> = self
            .outstanding
            .iter()
            .map(|item| cause(&item.warning))
            .collect();
        causes.sort_unstable();
        causes.dedup();
        causes
    }

    pub fn unresolved(&self) -> bool {
        self.episodes.unresolved()
    }

    pub fn checkpoint_recovery_id(&self) -> Result<(Episode, Option<i64>), String> {
        if self.owner_revision != self.sample_revision {
            return Err(
                "Operator input superseded this model response; checkpoint rejected.".into(),
            );
        }
        let Some(episode) = self.episodes.episode().filter(|_| self.unresolved()) else {
            return Err(
                "Recovery checkpoints can only be saved during an unresolved machine warning."
                    .into(),
            );
        };
        if self.wait_id().is_some() {
            return Err("Save the checkpoint before requesting a recovery wait.".into());
        }
        Ok((episode, self.recovery_id))
    }

    pub fn bind_checkpoint_recovery(&mut self, episode: Episode, recovery_id: i64) {
        if self.episodes.episode() == Some(episode)
            && self.unresolved()
            && self.recovery_id.is_none()
        {
            self.recovery_id = Some(recovery_id);
        }
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
        now: Duration,
    ) {
        self.interruptions
            .retain(|(at, _)| now.saturating_sub(*at) < HOUR);
        self.blocked_reason = None;
        let sample = match observation {
            Err(_) => Sample::Unavailable,
            Ok(status) => match self.evidence(status, config) {
                _ if !status.warnings.is_empty() => Sample::Warning,
                Err(reason) => {
                    self.blocked_reason = Some(reason.into());
                    if blocks(reason) {
                        Sample::Blocked
                    } else {
                        Sample::Unavailable
                    }
                }
                Ok(false) => Sample::Short,
                Ok(true) => Sample::Headroom {
                    required: Duration::from_secs(self.required_seconds(config)),
                },
            },
        };
        let Ok(observed) = self.episodes.observe(now, sample) else {
            return;
        };
        if observed.opened {
            self.recovery_id = None;
            self.outstanding.clear();
            self.interruptions.push_back((now, Vec::new()));
            // Bounded even for pathological warning flapping.
            while self.interruptions.len() > 256 {
                self.interruptions.pop_front();
            }
        }
        let Ok(status) = observation else {
            return;
        };
        for warning in &status.warnings {
            if !self
                .outstanding
                .iter()
                .any(|old| same_condition(&old.warning, warning))
            {
                self.outstanding.push(Outstanding::new(warning.clone()));
            }
            if let Some((_, causes)) = self.interruptions.back_mut()
                && !causes.contains(&cause(warning))
            {
                causes.push(cause(warning));
            }
        }
    }

    fn evidence(
        &mut self,
        status: &MachineStatus,
        config: &MachineWarningsConfig,
    ) -> Result<bool, &'static str> {
        let verdicts: Vec<_> = self
            .outstanding
            .iter_mut()
            .map(|item| recovered(&item.warning, &mut item.side, status, config))
            .collect();
        verdicts
            .into_iter()
            .try_fold(true, |healthy, next| next.map(|next| healthy && next))
    }

    /// Thermal recovery needs a long window; power and disk flip instantly.
    pub(super) fn required_seconds(&self, config: &MachineWarningsConfig) -> u64 {
        if self
            .outstanding
            .iter()
            .any(|item| cause(&item.warning) == "thermal")
        {
            config.recovery_stable_seconds
        } else {
            config.recovery_instant_seconds
        }
    }

    pub fn status(&self, config: &MachineWarningsConfig, now: Duration) -> RecoveryStatus {
        let interruptions_last_hour = ["power", "thermal", "disk"]
            .into_iter()
            .map(|cause| Interruption {
                cause,
                count: self
                    .interruptions
                    .iter()
                    .filter(|(at, causes)| {
                        now.saturating_sub(*at) < HOUR && causes.contains(&cause)
                    })
                    .count(),
            })
            .collect();
        RecoveryStatus {
            phase: self.phase(),
            generation: self.generation(),
            recovery_id: self.recovery_id,
            stable_seconds: self.episodes.stable_for().map_or(0, |held| held.as_secs()),
            required_seconds: self.required_seconds(config),
            wait_id: self.wait_id().map(str::to_owned),
            outstanding: self
                .outstanding
                .iter()
                .map(|item| item.warning.clone())
                .collect(),
            interruptions_last_hour,
            blocked_reason: self.blocked_reason.clone(),
        }
    }

    /// Fixed vocabulary only: OS names/paths must never become instructions.
    pub fn instruction(&self, config: &MachineWarningsConfig, now: Duration) -> Option<String> {
        if self.outstanding.is_empty() {
            return None;
        }
        let status = self.status(config, now);
        let state = match self.phase() {
            Phase::Recovered => {
                "Recovery confirmed by stable observations. Assess workload sustainability \
                 before resuming; reduce workload or stop as needed. \
                 Read saved notes from chaos://recovery; \
                 they are historical data, not health evidence."
            }
            Phase::Stabilizing => {
                "Recovery observations are stabilizing. \
                 Keep interruption-sensitive/heavy work paused."
            }
            Phase::Unavailable => {
                "Recovery measurements unavailable. \
                 Verify power, storage, and cooling before resuming."
            }
            Phase::Blocked => {
                "Recovery target exceeds resource capacity; \
                 operator configuration or resource changes are required."
            }
            _ => {
                "Machine warning remains unresolved; keep interruption-sensitive/heavy work paused."
            }
        };
        Some(format!(
            "Machine recovery (harness host): {state}\n\
             🩺 Kernel health checks and notifications are automatic. \
             Required stable observation: {} seconds. \
             Recent warning episodes (last hour): power {}, thermal {}, disk {}. \
             Repeated thermal interruptions warrant reducing workload or stopping. \
             Heat within configured/OS thresholds is expected. Report changes once. \
             While unresolved, save a concise note with save_recovery_checkpoint if available \
             and verify its database acknowledgement; do not scatter checkpoint files. \
             If persistence fails, tell the operator rather than claiming the note was saved. \
             Stop/check your heavy background processes, then call wait_for_machine_recovery alone \
             to opt into one recovery wake. Existing processes remain under operator control.",
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

fn blocks(reason: &str) -> bool {
    matches!(
        reason,
        "recovery target exceeds filesystem capacity" | "recovery target exceeds battery capacity"
    )
}

fn alarmed(warning: &MachineWarning) -> Band {
    match warning {
        MachineWarning::Thermal { .. } | MachineWarning::CpuTemperature { .. } => Band::High,
        MachineWarning::LowBattery { .. } | MachineWarning::LowDiskSpace { .. } => Band::Low,
    }
}

fn recovered(
    warning: &MachineWarning,
    side: &mut Band,
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
                + u16::from(config.recovery_battery_margin_percent);
            let thresholds = Thresholds::new(u16::from(config.battery_percent), target)
                .filter(|_| target <= 100)
                .ok_or("recovery target exceeds battery capacity")?;
            let Ok(band) = Hysteresis::restored(thresholds, *side).observe(u16::from(charge));
            *side = band;
            Ok(band == Band::High)
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
            let band = Thresholds::new(
                threshold - config.recovery_temperature_margin_celsius,
                *threshold,
            )
            .and_then(|thresholds| {
                Hysteresis::restored(thresholds, *side)
                    .observe(reading)
                    .ok()
            })
            .ok_or("triggering temperature sensor unavailable")?;
            *side = band;
            Ok(band == Band::Low)
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
            let reserves = DiskThresholds::new(config, fs.total_bytes);
            let thresholds = Thresholds::new(reserves.warning_bytes, reserves.recovery_bytes?)
                .ok_or("recovery target exceeds filesystem capacity")?;
            let Ok(band) =
                Hysteresis::restored(thresholds, *side).observe(u128::from(fs.available_bytes));
            *side = band;
            Ok(band == Band::High)
        }
    }
}

#[cfg(test)]
mod tests;
