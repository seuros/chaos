#[cfg(target_os = "freebsd")]
mod abi;
#[cfg(target_os = "freebsd")]
mod acpi;
mod display;
mod firmware;
mod power;
mod thermal;
#[cfg(target_os = "freebsd")]
pub(crate) use thermal::read as read_thermal;

#[cfg(target_os = "freebsd")]
pub(crate) fn detect() -> (crate::MachineProfile, crate::PowerInfo) {
    use crate::sysctl::{integer, string};
    use crate::{ExecutionEnvironment, MachineProfile, PowerInfo};

    let jailed = integer(c"security.jail.jailed");
    if jailed != Some(0) {
        return (
            MachineProfile {
                execution_environment: if jailed == Some(1) {
                    ExecutionEnvironment::Container
                } else {
                    ExecutionEnvironment::Unknown
                },
                ..MachineProfile::default()
            },
            PowerInfo::default(),
        );
    }
    let execution_environment = match string(c"kern.vm_guest").as_deref() {
        Some("none") => ExecutionEnvironment::NoneDetected,
        Some(_) => ExecutionEnvironment::VirtualMachine,
        None => ExecutionEnvironment::Unknown,
    };
    let power = power::read();
    let mut profile = MachineProfile {
        model: firmware::read(c"smbios.system.product").and_then(firmware::model),
        displays: display::read(),
        execution_environment,
        freebsd_acpi: Some(acpi::read()),
        ..MachineProfile::default()
    };
    if execution_environment == ExecutionEnvironment::NoneDetected {
        let chassis = firmware::read(c"smbios.chassis.type")
            .as_deref()
            .and_then(firmware::chassis_code);
        (profile.form_factor, profile.form_factor_evidence) =
            crate::profile::classify(chassis, profile.model.as_deref(), &power);
    }
    (profile, power)
}

#[cfg(test)]
mod tests;
