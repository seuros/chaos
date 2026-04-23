pub mod accounts;
pub mod debug_sandbox;
mod exit_status;

use chaos_getopt::CliConfigOverrides;
use clap::Parser;

#[derive(Debug, Parser)]
pub struct SeatbeltCommand {
    /// Convenience alias for low-friction sandboxed automatic execution (network-disabled sandbox that can write to cwd and TMPDIR)
    #[arg(long = "full-auto", default_value_t = false)]
    pub full_auto: bool,

    /// While the command runs, capture macOS sandbox denials via `log stream` and print them after exit
    #[arg(long = "log-denials", default_value_t = false)]
    pub log_denials: bool,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Full command args to run under seatbelt.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Parser)]
pub struct LandlockCommand {
    /// Convenience alias for low-friction sandboxed automatic execution (network-disabled sandbox that can write to cwd and TMPDIR)
    #[arg(long = "full-auto", default_value_t = false)]
    pub full_auto: bool,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Full command args to run under the Linux sandbox.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

/// Platform-agnostic sandbox command. Parsed identically on all platforms;
/// dispatch to seatbelt or landlock happens at runtime based on cfg.
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

impl SandboxCommand {
    /// Convert into the platform-specific command struct.
    #[cfg(target_os = "macos")]
    pub fn into_seatbelt(self) -> SeatbeltCommand {
        SeatbeltCommand {
            full_auto: self.full_auto,
            log_denials: self.log_denials,
            config_overrides: self.config_overrides,
            command: self.command,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn into_landlock(self) -> LandlockCommand {
        LandlockCommand {
            full_auto: self.full_auto,
            config_overrides: self.config_overrides,
            command: self.command,
        }
    }

    #[cfg(target_os = "freebsd")]
    pub fn into_capsicum(self) -> LandlockCommand {
        LandlockCommand {
            full_auto: self.full_auto,
            config_overrides: self.config_overrides,
            command: self.command,
        }
    }
}
