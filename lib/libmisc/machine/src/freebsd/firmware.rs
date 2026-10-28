#[cfg(target_os = "freebsd")]
pub(super) fn read(name: &std::ffi::CStr) -> Option<String> {
    // Query only the requested public identity field, never dump kenv or read
    // serial numbers. kenv can truncate successfully, so require the final NUL.
    let mut buffer = [0xff_u8; 1024];
    // SAFETY: name is terminated and buffer is writable for the declared size.
    let len = unsafe {
        libc::kenv(
            libc::KENV_GET,
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len() as libc::c_int,
        )
    };
    let len = usize::try_from(len).ok()?;
    let value = std::ffi::CStr::from_bytes_with_nul(buffer.get(..len)?).ok()?;
    Some(value.to_str().ok()?.trim().to_owned())
}

pub(super) fn model(value: String) -> Option<String> {
    let value = value.trim();
    let placeholder = [
        "",
        "unknown",
        "none",
        "not specified",
        "default string",
        "system product name",
        "to be filled by o.e.m.",
    ]
    .iter()
    .any(|placeholder| value.eq_ignore_ascii_case(placeholder));
    (!placeholder).then(|| value.to_owned())
}

pub(super) fn chassis_code(value: &str) -> Option<u8> {
    // stand/libsa/smbios.c exports these names rather than the SMBIOS byte.
    // Translate once, then reuse the shared chassis classifier.
    const NAMES: &[&str] = &[
        "Other",
        "Unknown",
        "Desktop",
        "Low Profile Desktop",
        "Pizza Box",
        "Mini Tower",
        "Tower",
        "Portable",
        "Laptop",
        "Notebook",
        "Hand Held",
        "Docking Station",
        "All in One",
        "Sub Notebook",
        "Lunch Box",
        "Space-saving",
        "Main Server Chassis",
        "Expansion Chassis",
        "SubChassis",
        "Bus Expansion Chassis",
        "Peripheral Chassis",
        "RAID Chassis",
        "Rack Mount Chassis",
        "Sealed-case PC",
        "Multi-system chassis",
        "Compact PCI",
        "Advanced TCA",
        "Blade",
        "Blade Enclosure",
        "Tablet",
        "Convertible",
        "Detachable",
        "IoT Gateway",
        "Embedded PC",
        "Mini PC",
        "Stick PC",
    ];
    NAMES
        .iter()
        .position(|name| value.trim().eq_ignore_ascii_case(name))
        .map(|index| (index + 1) as u8)
}
