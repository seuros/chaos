use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Host warning policy, separate from machine detection and execution permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct MachineWarningsConfig {
    /// Inject current warnings before normal model requests. Default: true.
    pub enabled: bool,
    /// Maximum wait for observations, including queued probes. Default: 3000 ms.
    #[serde(deserialize_with = "probe_timeout")]
    #[schemars(range(min = 1, max = 60000))]
    pub probe_timeout_ms: u64,
    /// Warn for a discharging system/UPS battery at or below this percentage.
    #[serde(deserialize_with = "percentage")]
    #[schemars(range(min = 0, max = 100))]
    pub battery_percent: u8,
    /// Warn for a relevant filesystem at or below this available percentage.
    #[serde(deserialize_with = "percentage")]
    #[schemars(range(min = 0, max = 100))]
    pub disk_free_percent: u8,
    /// Warn on OS thermal warnings or a CPU channel reaching its own critical limit.
    pub thermal: bool,
    /// Optional additional threshold for physical CPU temperatures only.
    #[serde(deserialize_with = "temperature")]
    #[schemars(range(min = -100, max = 250))]
    pub cpu_temperature_celsius: Option<f64>,
}

impl Default for MachineWarningsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            probe_timeout_ms: 3_000,
            battery_percent: 5,
            disk_free_percent: 5,
            thermal: true,
            cpu_temperature_celsius: None,
        }
    }
}

fn percentage<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let value = u8::deserialize(deserializer)?;
    if value > 100 {
        return Err(serde::de::Error::custom("percentage must be in 0..=100"));
    }
    Ok(value)
}

fn probe_timeout<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = u64::deserialize(deserializer)?;
    if !(1..=60_000).contains(&value) {
        return Err(serde::de::Error::custom(
            "probe timeout must be in 1..=60000 ms",
        ));
    }
    Ok(value)
}

fn temperature<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    let value = Option::<f64>::deserialize(deserializer)?;
    if value.is_some_and(|value| !(-100.0..=250.0).contains(&value)) {
        return Err(serde::de::Error::custom(
            "CPU temperature threshold must be finite and in -100..=250 Celsius",
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests;
