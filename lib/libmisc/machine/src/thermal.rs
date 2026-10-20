use crate::ExecutionEnvironment;
use serde::Serialize;

/// Strongest recognized system-wide OS thermal warning/pressure state.
/// Never inferred from individual temperatures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThermalState {
    /// Unavailable, unsupported, or unrecognized; not evidence of safe cooling.
    #[default]
    Unknown,
    /// The OS reports no thermal warning, not a guarantee about every component.
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureSource {
    Hwmon,
    ThermalZone,
    Sysctl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureKind {
    Physical,
    /// A driver-defined control scale, such as AMD Tctl, not die temperature.
    Control,
    Unknown,
}

/// One CPU-associated channel, not an average or a unique physical sensor.
#[derive(Debug, Clone, Serialize)]
pub struct CpuTemperature {
    /// OS-local sysfs path or sysctl name; not stable across boots/namespaces.
    pub id: String,
    pub label: String,
    pub source: TemperatureSource,
    pub kind: TemperatureKind,
    /// Celsius, or degree units on a control scale when `kind` is `Control`.
    /// Missing, unreadable, disabled, faulted, or implausible values stay `None`.
    pub celsius: Option<f64>,
    /// Driver/OS-reported limit on this channel's scale, not a configured policy.
    pub critical_celsius: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ThermalInfo {
    pub state: ThermalState,
    /// Best-effort CPU channels only. Empty means none could be identified,
    /// not a cool CPU or a complete hardware inventory. Channels may overlap.
    pub cpu_temperatures: Vec<CpuTemperature>,
    /// Raw macOS observations from this sample, not additional safety evidence.
    /// Absent on other platforms and when host observations are suppressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub macos: Option<MacosThermalDiagnostics>,
}

/// Preserve the two macOS sources separately so conflicting or process-local
/// observations remain visible rather than only their merged severity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MacosThermalDiagnostics {
    /// IOReturn from IOPMGetThermalWarningLevel; zero means the query succeeded.
    pub iopm_return_code: i32,
    /// Raw successful IOPM level: 0 normal, 100 danger, 10 crisis. Other values
    /// are unrecognized. None means the query failed, not a normal reading.
    pub iopm_warning_level: Option<u32>,
    /// Raw NSProcessInfo value: 0 nominal (also returned when unsupported),
    /// 1 fair, 2 serious, 3 critical. Unknown values are preserved verbatim.
    pub ns_process_info_thermal_state: isize,
}

pub(crate) fn inspect(environment: ExecutionEnvironment) -> ThermalInfo {
    if environment == ExecutionEnvironment::Container {
        return ThermalInfo::default();
    }
    crate::platform::read_thermal()
}

// Generous CPU-sensor sanity bounds, not warning thresholds. Preserve valid
// zero/subzero readings and overheating; reject sentinel/garbage values.
#[cfg(any(target_os = "linux", target_os = "freebsd", test))]
pub(crate) fn valid_celsius(value: f64) -> Option<f64> {
    (-100.0..=250.0).contains(&value).then_some(value)
}

#[cfg(test)]
mod tests;
