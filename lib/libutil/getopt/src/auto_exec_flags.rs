use chaos_ipc::protocol::ApprovalPolicy;
use chaos_ipc::protocol::GranularApprovalConfig;

/// Shared `--full-auto` and `--headless` flags scoped to the command that
/// flattens them.
#[derive(Debug, Clone, Default, usage::Args)]
pub struct AutoExecFlags {
    /// Convenience alias for low-friction sandboxed automatic execution.
    /// Runs commands under a workspace-write sandbox without prompting the
    /// user — MCP elicitations still surface.
    #[usage(long = "full-auto")]
    pub full_auto: bool,

    /// Skip confirmation prompts and execute commands without sandboxing.
    /// The flag is not the danger; vague prompts are. "Clean that up",
    /// "trash this stuff", "reinstall everything" become irreversible
    /// actions when nothing is left to ask. Name your files, scope, and
    /// limits. Precision over paranoia.
    #[usage(long = "headless", alias = "yolo", conflicts = "--full-auto")]
    pub headless: bool,
}

/// Shared `--full-auto` and `--headless` flags that usage propagates to
/// subcommands. Use only on command trees where execution-mode flags are
/// valid everywhere below the root.
#[derive(Debug, Clone, Default, usage::Args)]
pub struct GlobalAutoExecFlags {
    /// Convenience alias for low-friction sandboxed automatic execution.
    /// Runs commands under a workspace-write sandbox without prompting the
    /// user — MCP elicitations still surface.
    #[usage(long = "full-auto", global)]
    pub full_auto: bool,

    /// Skip confirmation prompts and execute commands without sandboxing.
    /// The flag is not the danger; vague prompts are. "Clean that up",
    /// "trash this stuff", "reinstall everything" become irreversible
    /// actions when nothing is left to ask. Name your files, scope, and
    /// limits. Precision over paranoia.
    #[usage(long = "headless", alias = "yolo", global, conflicts = "--full-auto")]
    pub headless: bool,
}

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
