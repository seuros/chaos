//! Warden private devfs setup. Never adds/deletes a global rule or changes a host
//! mount's active ruleset. Requires the operator's existing hide-only set 1.

use std::collections::BTreeSet;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(super) const RULESET: &str = "1";
const MAGIC: u32 = 0xdb0a087a;
const SET: u32 = 1 << 16;
const ALLOWED: [&str; 4] = ["null", "random", "urandom", "zero"];

// <fs/devfs/devfs.h>. The native-header regression checks the complete ABI.
#[repr(C)]
#[derive(Default)]
struct Rule {
    magic: u32,
    id: u32,
    conditions: i32,
    device_flags: i32,
    pattern: Pattern,
    actions: i32,
    boolean_actions: i32,
    uid: libc::uid_t,
    gid: libc::gid_t,
    mode: libc::mode_t,
    include_set: u16,
}

#[repr(transparent)]
struct Pattern([u8; 200]);
impl Default for Pattern {
    fn default() -> Self {
        Self([0; 200])
    }
}

const fn request(direction: libc::c_ulong, number: libc::c_ulong) -> libc::c_ulong {
    direction
        | ((std::mem::size_of::<Rule>() as libc::c_ulong) << 16)
        | (b'D' as libc::c_ulong) << 8
        | number
}
const GET_NEXT: libc::c_ulong = request(0xc0000000, 4);
const APPLY: libc::c_ulong = request(0x80000000, 2);

pub(super) fn verify_hide_only(directory: &File) -> io::Result<()> {
    let mut rule = Rule {
        magic: MAGIC,
        id: SET,
        ..Rule::default()
    };
    call(directory, GET_NEXT, &mut rule)?;
    if rule.id >> 16 != 1 || rule.conditions != 0 || rule.actions != 1 || rule.boolean_actions != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "devfs ruleset 1 must contain only unconditional hide",
        ));
    }
    match call(directory, GET_NEXT, &mut rule) {
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(()),
        Err(error) => Err(error),
        Ok(()) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "devfs ruleset 1 has additional rules",
        )),
    }
}

pub(super) fn configure(directory: &File, path: &Path) -> io::Result<()> {
    verify_hide_only(directory)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "untrusted private devfs root",
        ));
    }
    for name in ALLOWED {
        let mut rule = Rule {
            magic: MAGIC,
            conditions: 2,
            actions: 1,
            boolean_actions: 2,
            ..Rule::default()
        };
        rule.pattern.0[..name.len()].copy_from_slice(name.as_bytes());
        // APPLY changes only this private mount's current entries. The active
        // hide-only ruleset remains in force for subsequently created devices.
        call(directory, APPLY, &mut rule)?;
    }
    verify_hide_only(directory)?;
    let actual: BTreeSet<_> = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<_>>()?;
    let expected: BTreeSet<_> = ALLOWED.into_iter().map(std::ffi::OsString::from).collect();
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private devfs exposes unexpected entries",
        ));
    }
    for name in ALLOWED {
        if !std::fs::metadata(path.join(name))?
            .file_type()
            .is_char_device()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected minimal device type",
            ));
        }
    }
    Ok(())
}

fn call(directory: &File, command: libc::c_ulong, rule: &mut Rule) -> io::Result<()> {
    if unsafe { libc::ioctl(directory.as_raw_fd(), command, std::ptr::from_mut(rule)) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn devfs_abi_matches_native_headers() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("devfs-abi");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/devfs_abi.c");
        assert!(
            std::process::Command::new("cc")
                .args(["-Wall", "-Wextra", "-Werror"])
                .arg(source)
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        let output = std::process::Command::new(executable).output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "{} {} {} {} {} {}\n",
                std::mem::size_of::<Rule>(),
                std::mem::offset_of!(Rule, pattern),
                std::mem::offset_of!(Rule, actions),
                std::mem::offset_of!(Rule, include_set),
                APPLY,
                GET_NEXT
            )
        );
    }
}
