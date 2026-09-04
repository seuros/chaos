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

    /// Bearer token for `/api/trigger`. Falls back to `CHAOS_BEARER_TOKEN` env
    /// var. Empty tokens are rejected.
    #[usage(long = "bearer-token", env = "CHAOS_BEARER_TOKEN")]
    pub bearer_token: Option<String>,

    /// Per-trigger wall-clock timeout in seconds.
    #[usage(long = "timeout", default = "600")]
    pub timeout: u64,

    /// Maximum concurrent Chaos processes started by HTTP requests.
    #[usage(long = "max-concurrent", default = "4")]
    pub max_concurrent: usize,

    /// Maximum JSON request body size in bytes.
    #[usage(long = "body-limit", default = "1048576")]
    pub body_limit: usize,

    /// Model used for every trigger served by this process.
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
