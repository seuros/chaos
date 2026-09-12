use crate::ThermalState;

#[cfg(target_os = "macos")]
pub(crate) fn read() -> crate::ThermalInfo {
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPMGetThermalWarningLevel(level: *mut u32) -> i32;
    }

    let mut level = 255; // kIOPMThermalLevelUnknown
    // SAFETY: level points to writable uint32_t storage for this read-only query.
    let result = unsafe { IOPMGetThermalWarningLevel(&mut level) };
    let pressure = objc2_foundation::NSProcessInfo::processInfo()
        .thermalState()
        .0;
    crate::ThermalInfo {
        state: with_pressure(warning_state(result, level), pressure),
        // No stable public API for CPU Celsius readings; no private SMC keys.
        ..crate::ThermalInfo::default()
    }
}

fn warning_state(result: i32, level: u32) -> ThermalState {
    // IOPM.h's warning aliases are NOT numerically ordered by severity.
    // Failure (including an unpublished level) must never become Normal.
    match (result, level) {
        (0, 0) => ThermalState::Normal,    // kIOPMThermalWarningLevelNormal
        (0, 100) => ThermalState::Warning, // kIOPMThermalWarningLevelDanger
        (0, 10) => ThermalState::Critical, // kIOPMThermalWarningLevelCrisis
        _ => ThermalState::Unknown,
    }
}

fn with_pressure(warning: ThermalState, pressure: isize) -> ThermalState {
    // NSProcessInfo reports Nominal (0) even when unsupported. It can establish
    // elevated pressure, but zero alone cannot establish Normal. Preserve the
    // strongest recognized warning from either OS API.
    match (warning, pressure) {
        (ThermalState::Critical, _) | (_, 3) => ThermalState::Critical,
        (ThermalState::Warning, _) | (_, 1 | 2) => ThermalState::Warning,
        _ => warning,
    }
}

#[cfg(test)]
mod tests;
