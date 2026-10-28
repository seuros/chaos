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

/// Enumerate only one subtree through FreeBSD's native sysctl OID iterator.
/// Failure, malformed replies, or hitting the budget is not a complete tree.
#[cfg(target_os = "freebsd")]
pub(crate) fn names(root: &CStr, budget: usize) -> Option<Vec<std::ffi::CString>> {
    const MAX_NAME: usize = 24; // CTL_MAXNAME
    let mut prefix = [0_i32; MAX_NAME];
    let mut prefix_len = prefix.len();
    // SAFETY: prefix/length are writable, root is terminated.
    if unsafe { libc::sysctlnametomib(root.as_ptr(), prefix.as_mut_ptr(), &mut prefix_len) } != 0
        || !(1..=MAX_NAME).contains(&prefix_len)
    {
        return None;
    }
    let prefix = &prefix[..prefix_len];
    let mut cursor = prefix.to_vec();
    let mut names = Vec::new();
    for _ in 0..budget {
        let mut query = vec![0, 7]; // CTL_SYSCTL, CTL_SYSCTL_NEXTNOSKIP
        query.extend_from_slice(&cursor);
        let mut next = [0_i32; MAX_NAME];
        let mut size = size_of_val(&next);
        // SAFETY: query is a valid OID and next/size are bounded output buffers.
        let result = unsafe {
            libc::sysctl(
                query.as_ptr(),
                query.len() as u32,
                next.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return (std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT))
                .then_some(names);
        }
        if size == 0 || size > size_of_val(&next) || size % size_of::<i32>() != 0 {
            return None;
        }
        let next = &next[..size / size_of::<i32>()];
        if !next.starts_with(prefix) {
            return Some(names);
        }
        if next <= cursor.as_slice() {
            return None;
        }
        let mut query = vec![0, 1]; // CTL_SYSCTL, CTL_SYSCTL_NAME
        query.extend_from_slice(next);
        let mut bytes = [0_u8; 1024];
        let mut size = bytes.len();
        // SAFETY: the name query returns a bounded string, never a value.
        if unsafe {
            libc::sysctl(
                query.as_ptr(),
                query.len() as u32,
                bytes.as_mut_ptr().cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            return None;
        }
        let name = CStr::from_bytes_with_nul(bytes.get(..size)?).ok()?;
        let suffix = name.to_bytes().strip_prefix(root.to_bytes())?;
        if !suffix.starts_with(b".") {
            return None;
        }
        names.push(name.to_owned());
        cursor = next.to_vec();
    }
    None
}
