//! FreeBSD sandbox helper entry point.
//!
//! On FreeBSD, `alcatraz-freebsd` validates whether a requested sandbox policy
//! can be enforced safely by the current Capsicum-backed helper and then execs
//! the target command.
#[cfg(target_os = "freebsd")]
pub mod capsicum;
#[cfg(target_os = "freebsd")]
mod freebsd_run_main;

#[cfg(target_os = "freebsd")]
pub fn run_main() -> ! {
    freebsd_run_main::run_main();
}

#[cfg(not(target_os = "freebsd"))]
pub fn run_main() -> ! {
    panic!("alcatraz-freebsd is only supported on FreeBSD");
}

/// Register the platform credential store for FreeBSD.
///
/// Uses D-Bus Secret Service (via libdbus).
#[cfg(target_os = "freebsd")]
pub fn register_keyring_store() {
    use keyring_core::set_default_store;

    if let Ok(store) = dbus_secret_service_keyring_store::Store::new() {
        let _ = set_default_store(store);
    }
}

#[cfg(not(target_os = "freebsd"))]
pub fn register_keyring_store() {}
