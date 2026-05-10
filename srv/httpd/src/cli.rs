use std::path::PathBuf;

use clap::Parser;

/// CLI arguments for `chaos serve`.
#[derive(Debug, Parser)]
pub struct ServeCli {
    /// TCP bind address.
    #[arg(long = "bind", default_value = "127.0.0.1")]
    pub bind: String,

    /// TCP listen port.
    #[arg(long = "port", default_value_t = 4000)]
    pub port: u16,

    /// Bearer token for `/api/trigger`. Falls back to `CHAOS_BEARER_TOKEN` env
    /// var. Empty tokens are rejected.
    #[arg(long = "bearer-token", env = "CHAOS_BEARER_TOKEN")]
    pub bearer_token: Option<String>,

    /// Per-trigger wall-clock timeout in seconds.
    #[arg(long = "timeout", default_value_t = 600)]
    pub timeout: u64,

    /// Maximum concurrent Chaos processes started by HTTP requests.
    #[arg(long = "max-concurrent", default_value_t = 4)]
    pub max_concurrent: usize,

    /// Maximum JSON request body size in bytes.
    #[arg(long = "body-limit", default_value_t = 1_048_576)]
    pub body_limit: usize,

    /// Model used for every trigger served by this process.
    #[arg(short = 'm', long = "model")]
    pub model: Option<String>,

    /// Sandbox policy for model-generated commands.
    #[arg(long = "sandbox")]
    pub sandbox: Option<chaos_getopt::SandboxModeCliArg>,

    /// Match `chaos exec` behavior for headless runs outside trusted repos.
    #[arg(long = "skip-git-repo-check", default_value_t = false)]
    pub skip_git_repo_check: bool,

    /// Run without persisting session history.
    #[arg(long = "ephemeral", default_value_t = false)]
    pub ephemeral: bool,

    /// Working root for triggered processes.
    #[arg(short = 'C', long = "cd")]
    pub cd: Option<PathBuf>,
}
