//! macOS sandbox helper entry point.
//!
//! On macOS, `alcatraz-macos` is a thin helper that re-execs the trusted
//! `/usr/bin/sandbox-exec` binary with the Seatbelt arguments supplied by the
//! caller.

pub mod permissions;
#[cfg(target_os = "macos")]
pub mod seatbelt;
#[cfg(target_os = "macos")]
pub mod seatbelt_permissions;

pub mod protocol {
    pub use chaos_ipc::permissions::FileSystemSpecialPath;
    pub use chaos_ipc::protocol::*;
}

#[cfg(target_os = "macos")]
mod macos_run_main;

#[cfg(target_os = "macos")]
pub fn run_main() -> ! {
    macos_run_main::run_main();
}

#[cfg(not(target_os = "macos"))]
pub fn run_main() -> ! {
    panic!("alcatraz-macos is only supported on macOS");
}
