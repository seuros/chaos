use clap::Parser;
use std::collections::BTreeSet;
use std::env;
use std::ffi::CString;
use std::path::PathBuf;

use crate::landlock::apply_sandbox_policy_to_current_thread;
use alcatraz_base::sandbox_policy::{EffectiveSandboxPolicies, resolve_sandbox_policies};
use chaos_ipc::protocol::FileSystemSandboxPolicy;
use chaos_ipc::protocol::NetworkSandboxPolicy;
use chaos_ipc::protocol::SandboxPolicy;
use url::Url;

const MANAGED_PROXY_ENV_ERROR: &str = "managed proxy mode requires proxy environment variables";
const UNSUPPORTED_SPLIT_POLICY_ERROR: &str = "split filesystem sandbox policies that require direct runtime enforcement are not supported by the Linux sandbox backend";
const PROXY_ENV_KEYS: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "FTP_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "ftp_proxy",
    "YARN_HTTP_PROXY",
    "YARN_HTTPS_PROXY",
    "NPM_CONFIG_HTTP_PROXY",
    "NPM_CONFIG_HTTPS_PROXY",
    "NPM_CONFIG_PROXY",
    "npm_config_http_proxy",
    "npm_config_https_proxy",
    "npm_config_proxy",
    "BUNDLE_HTTP_PROXY",
    "BUNDLE_HTTPS_PROXY",
    "PIP_PROXY",
    "DOCKER_HTTP_PROXY",
    "DOCKER_HTTPS_PROXY",
];

#[derive(Debug, Parser)]
/// CLI surface for the Linux sandbox helper.
///
/// Applies landlock filesystem restrictions and seccomp syscall filters
/// in-process, then execs the target command.
pub struct LandlockCommand {
    /// It is possible that the cwd used in the context of the sandbox policy
    /// is different from the cwd of the process to spawn.
    #[arg(long = "sandbox-policy-cwd")]
    pub sandbox_policy_cwd: PathBuf,

    /// Legacy compatibility policy.
    ///
    /// Newer callers pass split filesystem/network policies as well so the
    /// helper can migrate incrementally without breaking older invocations.
    #[arg(long = "sandbox-policy", hide = true)]
    pub sandbox_policy: Option<SandboxPolicy>,

    #[arg(long = "file-system-sandbox-policy", hide = true)]
    pub file_system_sandbox_policy: Option<FileSystemSandboxPolicy>,

    #[arg(long = "network-sandbox-policy", hide = true)]
    pub network_sandbox_policy: Option<NetworkSandboxPolicy>,

    /// Internal compatibility flag.
    ///
    /// By default, restricted-network sandboxing uses isolated networking.
    /// If set, sandbox setup switches to proxy-only network mode.
    #[arg(long = "allow-network-for-proxy", hide = true, default_value_t = false)]
    pub allow_network_for_proxy: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "use-legacy-landlock", hide = true, default_value_t = false)]
    pub _use_legacy_landlock: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "apply-seccomp-then-exec", hide = true, default_value_t = false)]
    pub _apply_seccomp_then_exec: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "no-proc", hide = true, default_value_t = false)]
    pub _no_proc: bool,

    /// Accepted for backward compatibility but ignored.
    #[arg(long = "proxy-route-spec", hide = true)]
    pub _proxy_route_spec: Option<String>,

    /// Full command args to run under the Linux sandbox helper.
    #[arg(trailing_var_arg = true)]
    pub command: Vec<String>,
}

/// Entry point for the Linux sandbox helper.
///
/// 1. Apply landlock filesystem restrictions + seccomp syscall filters in-process.
/// 2. `execvp` into the final command.
pub fn run_main() -> ! {
    let LandlockCommand {
        sandbox_policy_cwd,
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
        allow_network_for_proxy,
        command,
        ..
    } = LandlockCommand::parse();

    if command.is_empty() {
        eprintln!("alcatraz-linux: no command specified to execute.");
        std::process::exit(1);
    }

    let EffectiveSandboxPolicies {
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
    } = resolve_sandbox_policies(
        sandbox_policy_cwd.as_path(),
        sandbox_policy,
        file_system_sandbox_policy,
        network_sandbox_policy,
    )
    .unwrap_or_else(|err| {
        eprintln!("alcatraz-linux: {err}");
        std::process::exit(1);
    });

    let allowed_proxy_ports = if allow_network_for_proxy {
        proxy_loopback_ports_from_env()
    } else {
        Vec::new()
    };
    if allow_network_for_proxy && allowed_proxy_ports.is_empty() {
        eprintln!("alcatraz-linux: {MANAGED_PROXY_ENV_ERROR}");
        std::process::exit(1);
    }

    if file_system_sandbox_policy
        .needs_direct_runtime_enforcement(network_sandbox_policy, sandbox_policy_cwd.as_path())
    {
        eprintln!("alcatraz-linux: {UNSUPPORTED_SPLIT_POLICY_ERROR}");
        std::process::exit(1);
    }

    let apply_landlock_fs =
        !file_system_sandbox_policy.has_full_disk_write_access() || allow_network_for_proxy;

    if let Err(e) = apply_sandbox_policy_to_current_thread(
        &sandbox_policy,
        network_sandbox_policy,
        &sandbox_policy_cwd,
        apply_landlock_fs,
        allow_network_for_proxy,
        /*proxy_routed_network*/ allow_network_for_proxy,
        &allowed_proxy_ports,
    ) {
        eprintln!("alcatraz-linux: {e}");
        std::process::exit(1);
    }

    exec_or_exit(command);
}

fn proxy_url_env_value(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .or_else(|| env::var(key.to_ascii_lowercase()).ok())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn proxy_scheme_default_port(scheme: &str) -> u16 {
    match scheme {
        "https" => 443,
        "socks5" | "socks5h" | "socks4" | "socks4a" => 1080,
        _ => 80,
    }
}

fn proxy_loopback_ports_from_env() -> Vec<u16> {
    let mut ports = BTreeSet::new();
    for key in PROXY_ENV_KEYS {
        let Some(proxy_url) = proxy_url_env_value(key) else {
            continue;
        };
        let trimmed = proxy_url.trim();
        if trimmed.is_empty() {
            continue;
        }

        let candidate = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        };
        let Ok(parsed) = Url::parse(&candidate) else {
            continue;
        };
        let Some(host) = parsed.host_str() else {
            continue;
        };
        if !is_loopback_host(host) {
            continue;
        }

        let scheme = parsed.scheme().to_ascii_lowercase();
        let port = parsed
            .port()
            .unwrap_or_else(|| proxy_scheme_default_port(scheme.as_str()));
        ports.insert(port);
    }
    ports.into_iter().collect()
}

/// Exec the provided argv, exiting with context if it fails.
fn exec_or_exit(command: Vec<String>) -> ! {
    #[expect(clippy::expect_used)]
    let c_command =
        CString::new(command[0].as_str()).expect("Failed to convert command to CString");
    #[expect(clippy::expect_used)]
    let c_args: Vec<CString> = command
        .iter()
        .map(|arg| CString::new(arg.as_str()).expect("Failed to convert arg to CString"))
        .collect();

    let mut c_args_ptrs: Vec<*const libc::c_char> = c_args.iter().map(|arg| arg.as_ptr()).collect();
    c_args_ptrs.push(std::ptr::null());

    unsafe {
        libc::execvp(c_command.as_ptr(), c_args_ptrs.as_ptr());
    }

    // If execvp returns, there was an error.
    let err = std::io::Error::last_os_error();
    eprintln!(
        "alcatraz-linux: failed to exec {}: {err}",
        command[0].as_str()
    );
    std::process::exit(1);
}

#[cfg(test)]
#[path = "linux_run_main_tests.rs"]
mod tests;
