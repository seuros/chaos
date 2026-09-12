use crate::power::percent;
use crate::{BatteryInfo, BatteryKind, BatteryState, PowerInfo, PowerSource};

mod thermal;
#[cfg(target_os = "macos")]
pub(crate) use thermal::read as read_thermal;

#[cfg(target_os = "macos")]
pub(crate) fn detect() -> (crate::MachineProfile, PowerInfo) {
    use crate::{ExecutionEnvironment, MachineProfile};

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGGetOnlineDisplayList(max: u32, displays: *mut u32, count: *mut u32) -> i32;
        fn CGSessionCopyCurrentDictionary() -> *const std::ffi::c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: *const std::ffi::c_void);
    }

    let model = crate::sysctl::string(c"hw.model");
    let virtual_machine = crate::sysctl::integer(c"kern.hv_vmm_present") == Some(1)
        || model
            .as_deref()
            .is_some_and(|model| model.starts_with("VirtualMac"));
    let power = read_power();
    let mut count = 0;
    // SAFETY: a zero-sized query accepts a null display array and writes count.
    let display_result = unsafe { CGGetOnlineDisplayList(0, std::ptr::null_mut(), &mut count) };
    // A disconnected/restricted WindowServer context can report no displays.
    // Only interpret zero as headless when a Quartz session is accessible.
    // SAFETY: Copy returns an owned CF dictionary or null.
    let session = unsafe { CGSessionCopyCurrentDictionary() };
    let session_available = !session.is_null();
    if session_available {
        // SAFETY: balance the successful Copy call exactly once.
        unsafe { CFRelease(session) };
    }
    let mut profile = MachineProfile {
        model,
        displays: display_state(display_result, count, session_available),
        execution_environment: if virtual_machine {
            ExecutionEnvironment::VirtualMachine
        } else {
            ExecutionEnvironment::NoneDetected
        },
        ..MachineProfile::default()
    };
    if !virtual_machine {
        (profile.form_factor, profile.form_factor_evidence) =
            crate::profile::classify(None, profile.model.as_deref(), &power);
    }
    (profile, power)
}

fn display_state(result: i32, count: u32, session_available: bool) -> crate::DisplayState {
    use crate::DisplayState;
    match (result, count, session_available) {
        (0, 0, true) => DisplayState::NoneDetected,
        (0, 1.., _) => DisplayState::Connected,
        _ => DisplayState::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn read_power() -> PowerInfo {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let Ok(mut child) = Command::new("/usr/bin/pmset")
        .args(["-g", "batt"])
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return PowerInfo::default();
    };
    // pmset's small output fits the pipe; a stalled/overproducing probe is
    // killed and reaped rather than delaying the caller indefinitely.
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return PowerInfo::default();
            }
        }
    }
    match child.wait_with_output() {
        Ok(output) if output.status.success() => {
            parse_power(&String::from_utf8_lossy(&output.stdout))
        }
        _ => PowerInfo::default(),
    }
}

fn parse_power(text: &str) -> PowerInfo {
    let header = text
        .lines()
        .find_map(|line| line.strip_prefix("Now drawing from "));
    let source = match header.map(str::trim) {
        Some("'AC Power'") => PowerSource::External,
        Some("'Battery Power'") => PowerSource::Battery,
        Some("'UPS Power'") => PowerSource::Ups,
        _ => return PowerInfo::default(),
    };
    let external_power = Some(source == PowerSource::External || text.contains("AC attached"));
    let mut batteries = Vec::new();
    let mut complete = true;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with('-'))
    {
        let kind = if line.starts_with("-InternalBattery") {
            BatteryKind::System
        } else if line.starts_with("-UPS") {
            BatteryKind::Ups
        } else {
            complete = false;
            continue;
        };
        let name = line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_start_matches('-');
        let present = !line.contains("present: false");
        let charge_percent = line.split_whitespace().find_map(|word| {
            word.trim_end_matches(';')
                .strip_suffix('%')
                .and_then(percent)
        });
        let state = line
            .split(';')
            .find_map(|field| match field.trim() {
                "charging" | "finishing charge" => Some(BatteryState::Charging),
                "discharging" => Some(BatteryState::Discharging),
                "charged" => Some(BatteryState::Full),
                "not charging" | "AC attached" => Some(BatteryState::Idle),
                _ => None,
            })
            .unwrap_or_default();
        batteries.push(BatteryInfo {
            name: name.to_owned(),
            kind,
            present: Some(present),
            charge_percent: if present { charge_percent } else { None },
            state: if present {
                state
            } else {
                BatteryState::Unknown
            },
        });
    }
    PowerInfo {
        // pmset explicitly reports the primary source. A battery's charging
        // state must not override that OS fact (e.g. AC with maintenance drain).
        source,
        external_power,
        batteries: complete.then_some(batteries),
    }
}

#[cfg(test)]
mod tests;
