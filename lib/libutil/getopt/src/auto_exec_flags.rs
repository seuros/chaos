use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::GranularApprovalConfig;

macro_rules! define_auto_exec_flags {
    ($(#[$meta:meta])* $name:ident, global = $global:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, clap::Args)]
        pub struct $name {
            /// Convenience alias for low-friction sandboxed automatic execution.
            /// Runs commands under a workspace-write sandbox without prompting the
            /// user — MCP elicitations still surface.
            #[arg(long = "full-auto", default_value_t = false, global = $global)]
            pub full_auto: bool,

            /// Skip confirmation prompts and execute commands without sandboxing.
            /// The flag is not the danger; vague prompts are. "Clean that up",
            /// "trash this stuff", "reinstall everything" become irreversible
            /// actions when nothing is left to ask. Name your files, scope, and
            /// limits. Precision over paranoia.
            #[arg(
                long = "headless",
                alias = "yolo",
                default_value_t = false,
                global = $global,
                conflicts_with = "full_auto"
            )]
            pub headless: bool,
        }
    };
}

define_auto_exec_flags!(
    /// Shared `--full-auto` and `--headless` flags scoped to the command that
    /// flattens them.
    AutoExecFlags,
    global = false
);

define_auto_exec_flags!(
    /// Shared `--full-auto` and `--headless` flags that clap propagates to
    /// subcommands. Use only on command trees where execution-mode flags are
    /// valid everywhere below the root.
    GlobalAutoExecFlags,
    global = true
);

/// Approval policy shared by `--full-auto` and `--headless`: never prompt the
/// user except for MCP elicitations.
pub fn auto_exec_approval_policy() -> ApprovalPolicy {
    ApprovalPolicy::Granular(GranularApprovalConfig {
        sandbox_approval: false,
        rules: false,
        request_permissions: false,
        mcp_elicitations: true,
    })
}
