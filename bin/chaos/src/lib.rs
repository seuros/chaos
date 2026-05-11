pub mod accounts;
pub mod debug_sandbox;
mod exit_status;

use chaos_getopt::CliConfigOverrides;
use clap::Parser;

/// Platform-agnostic sandbox command. Parsed identically on all platforms;
/// dispatch to seatbelt/landlock/capsicum happens at runtime based on cfg.
#[derive(Debug, Parser)]
pub struct SandboxCommand {
    /// Convenience alias for low-friction sandboxed automatic execution (network-disabled sandbox that can write to cwd and TMPDIR)
    #[arg(long = "full-auto", default_value_t = false)]
    pub full_auto: bool,

    /// While the command runs, capture macOS sandbox denials via `log stream` and print them after exit (macOS only)
    #[cfg(target_os = "macos")]
    #[arg(long = "log-denials", default_value_t = false)]
    pub log_denials: bool,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Command to run inside the sandbox.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}
