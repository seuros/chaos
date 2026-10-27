//! ABI-checked interface to mac_gulag: Glorious Unified Labor And Governance.
//! ABI 1 enforces network/process restrictions, never filesystem paths.
//! Never loads a module, invokes sudo, or changes host-wide policy.

use alcatraz_base::error::{AlcatrazError, Result};
use chaos_ipc::protocol::{SocketPolicy, VfsPolicy};
use chaos_parole::sandbox::{has_full_disk_read_access, has_full_disk_write_access};
use std::ffi::CString;
use std::io;
use std::path::Path;

const ABI_VERSION: i32 = 1;
const LABEL_ELEMENT: &[u8] = b"gulag";
const LABEL_PREFIX: &str = "gulag/";

#[repr(C)]
struct Mac {
    buflen: usize,
    string: *mut libc::c_char,
}

unsafe extern "C" {
    fn mac_set_proc(label: *const Mac) -> libc::c_int;
    fn mac_get_proc(label: *mut Mac) -> libc::c_int;
}

fn unsupported(message: impl Into<String>) -> AlcatrazError {
    AlcatrazError::UnsupportedOperation(message.into())
}

pub(crate) fn apply(vfs: &VfsPolicy, network: SocketPolicy, cwd: &Path) -> Result<()> {
    build_label(vfs, network, cwd)?;
    let mut policy = PreparedPolicy::new(network)?;
    // SAFETY: this helper performs confinement immediately before exec.
    unsafe { policy.apply_in_child()? };
    Ok(())
}

/// Allocate and validate everything before fork. Adoption/readback in the
/// trusted child uses only the MAC syscalls and a fixed-size stack buffer.
pub(crate) struct PreparedPolicy {
    text: CString,
}

impl PreparedPolicy {
    pub(crate) fn new(network: SocketPolicy) -> Result<Self> {
        if sysctl_int(c"security.mac.gulag.abi_version") != Some(ABI_VERSION)
            || sysctl_int(c"security.mac.gulag.enabled") != Some(1)
        {
            return Err(unsupported(
                "FreeBSD network/process restrictions require mac_gulag ABI 1; \
             the module is not loaded automatically. No restrictions were bypassed",
            ));
        }
        let text = CString::new(process_label(network))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in MAC label"))?;
        Ok(Self { text })
    }

    /// # Safety
    /// Use only during trusted setup, after identity/jail changes and before
    /// executing any command. Failure must abort execution.
    pub(crate) unsafe fn apply_in_child(&mut self) -> io::Result<()> {
        let request = Mac {
            buflen: self.text.as_bytes_with_nul().len(),
            string: self.text.as_ptr().cast_mut(),
        };
        // SAFETY: request and its NUL-terminated string live through the syscall.
        if unsafe { mac_set_proc(&request) } != 0 {
            return Err(io::Error::last_os_error());
        }

        // Verify the credential actually contains the exact policy we installed.
        let mut buffer = [0_u8; 128];
        buffer[..LABEL_ELEMENT.len()].copy_from_slice(LABEL_ELEMENT);
        let mut result = Mac {
            buflen: buffer.len(),
            string: buffer.as_mut_ptr().cast(),
        };
        // SAFETY: result points to a writable, bounded label buffer.
        if unsafe { mac_get_proc(&mut result) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let actual = buffer.split(|byte| *byte == 0).next().unwrap_or_default();
        if actual != self.text.as_bytes() {
            // Raw errno avoids formatting/allocation in a post-fork callback.
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        Ok(())
    }
}

fn sysctl_int(name: &std::ffi::CStr) -> Option<i32> {
    let mut value = 0_i32;
    let mut size = std::mem::size_of_val(&value);
    // SAFETY: name is NUL-terminated; value/size are correctly sized outputs.
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::addr_of_mut!(value).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of_val(&value)).then_some(value)
}

/// Direct MAC adoption cannot enforce filesystem restrictions. Warden RPC
/// happens before this path; reject rather than install a network-only label.
pub(crate) fn build_label(vfs: &VfsPolicy, network: SocketPolicy, _cwd: &Path) -> Result<String> {
    if !has_full_disk_read_access(vfs) {
        return Err(unsupported(
            "restricted filesystem reads require the FreeBSD Warden jail backend; \
             mac_gulag ABI 1 does not enforce filesystem paths",
        ));
    }
    if !has_full_disk_write_access(vfs) {
        return Err(unsupported(
            "restricted filesystem writes require a root-launched FreeBSD Warden connection; \
             none is configured for this helper; \
             mac_gulag ABI 1 is network/process-only",
        ));
    }
    Ok(process_label(network))
}

/// Shared with the Warden planner. Producing text is not enforcement.
pub(crate) fn process_label(network: SocketPolicy) -> String {
    format!(
        "{LABEL_PREFIX}restrict!v={ABI_VERSION}{}",
        if network.is_enabled() {
            ""
        } else {
            "!net=deny"
        }
    )
}

#[cfg(test)]
mod tests;
