//! Linux sandbox helper entry point.
//!
//! On Linux, `alcatraz-linux` applies landlock filesystem restrictions
//! and seccomp syscall filters in-process, then execs the target command.
#[cfg(target_os = "linux")]
mod landlock;
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
