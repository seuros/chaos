use std::path::PathBuf;

/// CLI arguments for `chaos serve`.
#[derive(Debug, usage::Args)]
pub struct ServeCli {
    /// TCP bind address.
    #[usage(long = "bind", default = "127.0.0.1")]
    pub bind: String,

    /// TCP listen port.
    #[usage(long = "port", default = "4000")]
    pub port: u16,

    /// Bearer token for triggers and live monitoring. Falls back to `CHAOS_BEARER_TOKEN` env
    /// var. Empty tokens are rejected.
    #[usage(long = "bearer-token", env = "CHAOS_BEARER_TOKEN")]
    pub bearer_token: Option<String>,

    /// Trigger wall-clock timeout, retained-turn deadline and body-read timeout, in seconds.
    #[usage(long = "timeout", default = "600")]
    pub timeout: u64,

    /// Maximum concurrent triggers/retained turns, including trigger body reading and cleanup.
    #[usage(long = "max-concurrent", default = "4")]
    pub max_concurrent: usize,

    /// Maximum JSON request body size in bytes.
    #[usage(long = "body-limit", default = "1048576")]
    pub body_limit: usize,

    /// Enable the experimental retained-session API alongside /api/trigger.
    #[usage(long = "sessions")]
    pub sessions: bool,

    /// Persist retained-session identity/outcomes in journald under this namespace.
    /// Requires --sessions and forbids --ephemeral.
    #[usage(long = "session-scope")]
    pub session_scope: Option<String>,

    /// Enable operator recovery mutations with a separate token.
    /// Requires --session-scope; also accepts CHAOS_RECOVERY_TOKEN.
    #[usage(long = "recovery-token", env = "CHAOS_RECOVERY_TOKEN")]
    pub recovery_token: Option<String>,

    /// Inspect/drain retained records without booting or admitting execution.
    /// Requires --session-scope and a separate --recovery-token.
    #[usage(long = "session-recovery-only")]
    pub session_recovery_only: bool,

    /// Deliver durable terminal events to this trusted, server-owned URL.
    /// HTTPS required, except literal loopback HTTP for local development.
    #[usage(long = "session-webhook-url")]
    pub session_webhook_url: Option<String>,

    /// Environment variable containing the webhook HMAC key (at least 32 bytes).
    #[usage(
        long = "session-webhook-secret-env",
        default = "CHAOS_SESSION_WEBHOOK_SECRET"
    )]
    pub session_webhook_secret_env: String,

    /// Maximum retained session records (including closed-session tombstones).
    #[usage(long = "max-sessions", default = "32")]
    pub max_sessions: usize,

    /// Maximum lifetime of a retained session in seconds.
    #[usage(long = "session-ttl", default = "3600")]
    pub session_ttl: u64,

    /// Default model for triggers and retained sessions.
    #[usage(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Sandbox policy for model-generated commands.
    #[usage(long = "sandbox", value_enum)]
    pub sandbox: Option<chaos_getopt::SandboxModeCliArg>,

    /// Match `chaos exec` behavior for headless runs outside trusted repos.
    #[usage(long = "skip-git-repo-check")]
    pub skip_git_repo_check: bool,

    /// Run without persisting session history.
    #[usage(long = "ephemeral")]
    pub ephemeral: bool,

    /// Working root for triggered processes.
    #[usage(short = 'C', long = "cd")]
    pub cd: Option<PathBuf>,
}
