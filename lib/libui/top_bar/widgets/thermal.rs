use chaos_kern::machine_status::{MachineStatus, MachineWarning};
use chaos_machine::{TemperatureKind, ThermalState};

use super::super::machine::Source;
use super::super::{BarWidget, Content, Tone};

pub(in crate::top_bar) fn new(source: Source) -> BarWidget {
    super::observed("thermal", 205, source, present)
}

fn present(status: &MachineStatus) -> Content {
    let thermal = &status.machine.thermal;
    let cpu_warning = status
        .warnings
        .iter()
        .any(|warning| matches!(warning, MachineWarning::CpuTemperature { .. }));
    let os_warning = status
        .warnings
        .iter()
        .any(|warning| matches!(warning, MachineWarning::Thermal { .. }));
    let tone = if cpu_warning || (os_warning && thermal.state == ThermalState::Critical) {
        Tone::Error
    } else if os_warning {
        Tone::Warning
    } else {
        Tone::Normal
    };
    let physical_max = thermal
        .cpu_temperatures
        .iter()
        .filter(|sensor| sensor.kind == TemperatureKind::Physical)
        .filter_map(|sensor| sensor.celsius)
        .filter(|value| value.is_finite())
        .max_by(f64::total_cmp);
    let text = match (thermal.state, physical_max) {
        (ThermalState::Critical, _) => "heat critical".into(),
        (ThermalState::Warning, _) => "heat warning".into(),
        _ if cpu_warning => "CPU limit".into(),
        (_, Some(value)) => format!("CPU max {value:.0}°C"),
        (ThermalState::Normal, None) => "heat normal".into(),
        (ThermalState::Unknown, None) => return Content::default(),
    };
    Content::new(text).tone(tone)
}
