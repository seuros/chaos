//! Small bindings to existing read-only FreeBSD ACPI/DRM ioctls.
//! Native-header regression checks keep the layouts and request sizes honest.

use std::io;
use std::os::fd::RawFd;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct BatteryInfo {
    pub cap: i32,
    pub minutes: i32,
    pub state: i32,
    pub rate: i32,
}

#[repr(C)]
pub(super) union BatteryArgument {
    pub unit: i32,
    pub info: BatteryInfo,
    // acpi_battery_ioctl_arg includes the 256-byte acpi_bix, even when
    // GET_BATTINFO only returns its 16-byte battinfo member.
    pub storage: [u32; 64],
}

#[repr(C)]
#[derive(Default)]
pub(super) struct CardResources {
    pub fb_ids: u64,
    pub crtc_ids: u64,
    pub connector_ids: u64,
    pub encoder_ids: u64,
    pub count_fbs: u32,
    pub count_crtcs: u32,
    pub count_connectors: u32,
    pub count_encoders: u32,
    pub min_width: u32,
    pub max_width: u32,
    pub min_height: u32,
    pub max_height: u32,
}

#[repr(C)]
#[derive(Default)]
pub(super) struct Connector {
    pub encoders: u64,
    pub modes: u64,
    pub properties: u64,
    pub property_values: u64,
    pub count_modes: u32,
    pub count_properties: u32,
    pub count_encoders: u32,
    pub encoder_id: u32,
    pub connector_id: u32,
    pub connector_type: u32,
    pub connector_type_id: u32,
    pub connection: u32,
    pub mm_width: u32,
    pub mm_height: u32,
    pub subpixel: u32,
    pub pad: u32,
}

const fn request<T>(direction: libc::c_ulong, group: u8, number: u8) -> libc::c_ulong {
    direction
        | ((size_of::<T>() as libc::c_ulong) << 16)
        | ((group as libc::c_ulong) << 8)
        | number as libc::c_ulong
}

pub(super) const BATTERY_UNITS: libc::c_ulong = request::<i32>(0x4000_0000, b'B', 1);
pub(super) const BATTERY_INFO: libc::c_ulong = request::<BatteryArgument>(0xc000_0000, b'B', 3);
pub(super) const CARD_RESOURCES: libc::c_ulong = request::<CardResources>(0xc000_0000, b'd', 0xa0);
pub(super) const CONNECTOR: libc::c_ulong = request::<Connector>(0xc000_0000, b'd', 0xa7);

/// # Safety
/// `value` must be the exact ABI structure for `request`. Any embedded output
/// pointers must remain valid, with their declared capacities, during the call.
pub(super) unsafe fn query<T>(fd: RawFd, request: libc::c_ulong, value: &mut T) -> io::Result<()> {
    // SAFETY: the caller establishes the request-specific ABI and pointer bounds.
    if unsafe { libc::ioctl(fd, request, std::ptr::from_mut(value)) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
