//! Linux sandbox helper entry point.
//!
//! On Linux, `alcatraz-linux` applies landlock filesystem restrictions
//! and seccomp syscall filters in-process, then execs the target command.
#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
mod linux_run_main;

#[cfg(target_os = "linux")]
pub fn run_main() -> ! {
    linux_run_main::run_main();
}

#[cfg(not(target_os = "linux"))]
pub fn run_main() -> ! {
    panic!("alcatraz-linux is only supported on Linux");
}

/// Register the platform credential store for Linux.
///
/// Tries kernel keyutils first (fast, in-kernel), falls back to D-Bus
/// Secret Service if keyutils is unavailable.
#[cfg(target_os = "linux")]
pub fn register_keyring_store() {
    use keyring_core::set_default_store;

    if let Ok(store) = linux_keyutils_keyring_store::Store::new() {
        set_default_store(store);
    } else if let Ok(store) = dbus_secret_service_keyring_store::Store::new() {
        set_default_store(store);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn register_keyring_store() {}
