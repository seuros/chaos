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
