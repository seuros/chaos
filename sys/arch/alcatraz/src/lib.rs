//! Compile-time facade for the host operating system's Alcatraz backend.
//!
//! A Chaos binary can execute on only one target OS, so this crate exposes one
//! statically selected backend instead of a runtime backend registry.

pub use alcatraz_base::prepared_command::PreparedCommand;
pub use alcatraz_base::prepared_command::SandboxRequest;

pub const HELPER_ARG0: &str = "alcatraz";

#[cfg(target_os = "freebsd")]
pub use alcatraz_freebsd::prepare_command;
#[cfg(target_os = "freebsd")]
pub use alcatraz_freebsd::register_keyring_store;
#[cfg(target_os = "freebsd")]
pub use alcatraz_freebsd::run_main;
#[cfg(target_os = "linux")]
pub use alcatraz_linux::landlock;
#[cfg(target_os = "linux")]
pub use alcatraz_linux::prepare_command;
#[cfg(target_os = "linux")]
pub use alcatraz_linux::register_keyring_store;
#[cfg(target_os = "linux")]
pub use alcatraz_linux::run_main;
#[cfg(target_os = "macos")]
pub use alcatraz_macos::prepare_command;
#[cfg(target_os = "macos")]
pub use alcatraz_macos::run_main;
#[cfg(target_os = "macos")]
pub fn register_keyring_store() {}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "freebsd")))]
compile_error!("Alcatraz has no sandbox backend for this target OS");
