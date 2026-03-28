use std::os::unix::process::CommandExt;
use std::process::Command;

const MACOS_PATH_TO_SEATBELT_EXECUTABLE: &str = "/usr/bin/sandbox-exec";

/// Replaces the current process with `/usr/bin/sandbox-exec`, forwarding the
/// caller-provided Seatbelt arguments verbatim.
pub fn run_main() -> ! {
    let exec_error = Command::new(MACOS_PATH_TO_SEATBELT_EXECUTABLE)
        .args(std::env::args_os().skip(1))
        .exec();
    eprintln!("alcatraz-macos: failed to exec {MACOS_PATH_TO_SEATBELT_EXECUTABLE}: {exec_error}");
    std::process::exit(1);
}
