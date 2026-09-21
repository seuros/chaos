//! Battery presentation consumes snapshots published by the shared runtime.

use chaos_kern::machine_status::{MachineStatus, MachineWarning};
use chaos_machine::{BatteryKind, FormFactor, PowerSource};

use super::super::machine::Source;
use super::super::{BarWidget, Content, Tone};

pub(in crate::top_bar) fn new(source: Source) -> BarWidget {
    super::observed("battery", 240, source, present)
}

fn present(status: &MachineStatus) -> Content {
    if status.machine.profile.form_factor == FormFactor::Desktop {
        return Content::default();
    }

    let power = &status.machine.power;
    let (icon, kind) = match (power.source, power.external_power) {
        // AC takes precedence over dead, removed, or maintenance-discharging batteries.
        (PowerSource::External, _) | (_, Some(true)) => return Content::new("⚡ AC"),
        (PowerSource::Battery, _) => ("●", BatteryKind::System),
        (PowerSource::Ups, _) => ("UPS", BatteryKind::Ups),
        (PowerSource::Unknown, _) => return Content::new("power ?"),
    };
    let levels: Vec<_> = power
        .batteries
        .iter()
        .flatten()
        .filter(|battery| battery.kind == kind && battery.present != Some(false))
        .map(|battery| {
            battery
                .charge_percent
                .map(|level| format!("{level}%"))
                .unwrap_or_else(|| "?".into())
        })
        .collect();
    // Keep individual percentages, never an invented combined capacity.
    let levels = if levels.is_empty() {
        "?".into()
    } else {
        levels.join("/")
    };
    let tone = if status
        .warnings
        .iter()
        .any(|warning| matches!(warning, MachineWarning::LowBattery { .. }))
    {
        Tone::Error
    } else {
        Tone::Normal
    };
    Content::new(format!("{icon} {levels}")).tone(tone)
}
