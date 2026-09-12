use std::ffi::CStr;

pub(crate) fn integer(name: &CStr) -> Option<i32> {
    let mut value = 0_i32;
    let mut size = size_of::<i32>();
    // SAFETY: both output pointers are valid for the declared sizes; read-only.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&mut value as *mut i32).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && size == size_of::<i32>()).then_some(value)
}

pub(crate) fn string(name: &CStr) -> Option<String> {
    // Model/guest strings are small. Reject truncation rather than using a
    // partial identifier as classification evidence.
    let mut bytes = [0_u8; 1024];
    let mut size = bytes.len();
    // SAFETY: the output buffer and its length are valid; read-only query.
    let result = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }
    let value = CStr::from_bytes_until_nul(bytes.get(..size)?).ok()?;
    let value = value.to_str().ok()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}
