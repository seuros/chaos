pub mod accounts;
pub mod debug_sandbox;
mod exit_status;

use chaos_getopt::CliConfigOverrides;

/// Platform-agnostic sandbox command. Parsed identically on all platforms;
/// dispatch to seatbelt/landlock/capsicum happens at runtime based on cfg.
#[derive(Debug, usage::Args)]
pub struct SandboxCommand {
    /// Convenience alias for low-friction sandboxed automatic execution (network-disabled sandbox that can write to cwd and TMPDIR)
    #[usage(long = "full-auto")]
    pub full_auto: bool,

    /// While the command runs, capture macOS sandbox denials via `log stream` and print them after exit (macOS only)
    #[cfg(target_os = "macos")]
    #[usage(long = "log-denials")]
    pub log_denials: bool,

    #[usage(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Command to run inside the sandbox.
    #[usage(trailing_var_arg)]
    pub command: Vec<String>,
}
