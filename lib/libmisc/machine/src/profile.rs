use crate::{BatteryKind, PowerInfo};
use serde::Serialize;
use std::io::IsTerminal;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormFactor {
    Laptop,
    Desktop,
    Server,
    Tablet,
    Other,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FormFactorEvidence {
    Firmware,
    ModelIdentifier,
    /// A heuristic, never used to infer desktop from an absent battery.
    SystemBattery,
    #[default]
    Unknown,
}

/// OS-reported displays, independent of graphical-session environment variables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayState {
    Connected,
    /// Headless according to the available display interface. Virtual displays
    /// count as connected; an inaccessible interface is `Unknown`, not headless.
    NoneDetected,
    #[default]
    Unknown,
}

/// Best-effort environment detection, not a security boundary or attestation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEnvironment {
    Container,
    VirtualMachine,
    /// No virtualization/container marker found; not proof of bare metal.
    NoneDetected,
    #[default]
    Unknown,
}

/// Session hints do not establish whether the machine itself is headless.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionContext {
    pub ssh: bool,
    pub graphical_hint: bool,
    pub terminal_attached: bool,
}

impl SessionContext {
    pub(crate) fn local() -> Self {
        fn set(name: &str) -> bool {
            std::env::var_os(name).is_some_and(|value| !value.is_empty())
        }
        Self {
            ssh: set("SSH_CONNECTION") || set("SSH_TTY"),
            graphical_hint: set("DISPLAY") || set("WAYLAND_DISPLAY"),
            terminal_attached: std::io::stdin().is_terminal(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MachineProfile {
    pub form_factor: FormFactor,
    pub form_factor_evidence: FormFactorEvidence,
    pub model: Option<String>,
    pub displays: DisplayState,
    pub execution_environment: ExecutionEnvironment,
    pub session: SessionContext,
}

pub(crate) fn classify(
    chassis: Option<u8>,
    model: Option<&str>,
    power: &PowerInfo,
) -> (FormFactor, FormFactorEvidence) {
    // SMBIOS chassis types. Unknown/unspecified values are not desktops.
    let firmware = match chassis {
        Some(3..=7 | 13 | 15 | 16 | 35 | 36) => Some(FormFactor::Desktop),
        Some(8..=11 | 14 | 31 | 32) => Some(FormFactor::Laptop),
        Some(17 | 23 | 28 | 29) => Some(FormFactor::Server),
        Some(30) => Some(FormFactor::Tablet),
        Some(1 | 12 | 18..=22 | 24..=27 | 33 | 34) => Some(FormFactor::Other),
        _ => None,
    };
    if let Some(form_factor) = firmware {
        return (form_factor, FormFactorEvidence::Firmware);
    }
    if let Some(model) = model {
        if model.starts_with("MacBook") || model.starts_with("PowerBook") {
            return (FormFactor::Laptop, FormFactorEvidence::ModelIdentifier);
        }
        if ["Macmini", "MacPro", "MacStudio", "iMac", "PowerMac"]
            .iter()
            .any(|prefix| model.starts_with(prefix))
        {
            return (FormFactor::Desktop, FormFactorEvidence::ModelIdentifier);
        }
    }
    if power.batteries.as_ref().is_some_and(|batteries| {
        batteries
            .iter()
            .any(|battery| battery.kind == BatteryKind::System && battery.present == Some(true))
    }) {
        return (FormFactor::Laptop, FormFactorEvidence::SystemBattery);
    }
    (FormFactor::Unknown, FormFactorEvidence::Unknown)
}

#[cfg(test)]
mod tests;
