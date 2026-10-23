//! Process ownership for operator-driven Antigravity examples.
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use futures::StreamExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

pub fn parameters() -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let server = args.next().map(PathBuf::from).ok_or("missing SERVER")?;
    let home = args
        .next()
        .map(PathBuf::from)
        .ok_or("missing PRIVATE_HOME")?;
    if args.next().is_some() || !server.is_absolute() || !home.is_absolute() {
        return Err("supply absolute SERVER and PRIVATE_HOME paths only".into());
    }
    let metadata = std::fs::symlink_metadata(&home)?;
    // SAFETY: geteuid has no preconditions.
    if !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("PRIVATE_HOME must be an owned, nonsymlink, mode-0700 directory".into());
    }
    if !server.with_file_name("localharness_external").is_file() {
        return Err("localharness_external must be adjacent to the official ACP server".into());
    }
    Ok((server, home))
}

pub struct Agent {
    pub child: Child,
    group: Option<i32>,
    diagnostics: Arc<AtomicU32>,
    diagnostic_reader: Option<JoinHandle<()>>,
}

impl Agent {
    pub fn spawn(server: &Path, home: &Path) -> io::Result<Self> {
        Self::spawn_inner(Command::new(server), home, None)
    }

    // Shared by opt-in kernel probes, not every executable importing this helper.
    #[allow(dead_code)]
    pub fn spawn_proxied(server: &Path, home: &Path, proxy: &str, ca: &Path) -> io::Result<Self> {
        if !proxy
            .strip_prefix("http://127.0.0.1:")
            .is_some_and(|port| port.parse::<u16>().is_ok_and(|port| port > 0))
            || !ca.is_absolute()
            || !ca.is_file()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected owned loopback proxy and absolute CA bundle",
            ));
        }
        Self::spawn_inner(Command::new(server), home, Some((proxy, ca)))
    }

    /// Test-only platform wrapper, with the same cleared environment as the control.
    #[allow(dead_code)]
    pub fn spawn_wrapped_proxied(
        server: &Path,
        home: &Path,
        proxy: &str,
        ca: &Path,
        wrapper: &Path,
        args: &[String],
    ) -> io::Result<Self> {
        if !proxy
            .strip_prefix("http://127.0.0.1:")
            .is_some_and(|port| port.parse::<u16>().is_ok_and(|port| port > 0))
            || !ca.is_absolute()
            || !ca.is_file()
            || !wrapper.is_absolute()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid constrained probe configuration",
            ));
        }
        let mut command = Command::new(wrapper);
        command.args(args).arg(server);
        Self::spawn_inner(command, home, Some((proxy, ca)))
    }

    fn spawn_inner(
        mut command: Command,
        home: &Path,
        egress: Option<(&str, &Path)>,
    ) -> io::Result<Self> {
        command
            .env_clear()
            .env("HOME", home)
            .env("GEMINI_HOME", home.join(".gemini"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("PATH", "/usr/bin:/bin");
        if let Some((proxy, ca)) = egress {
            for variable in [
                "HTTPS_PROXY",
                "HTTP_PROXY",
                "https_proxy",
                "http_proxy",
                "grpc_proxy",
            ] {
                command.env(variable, proxy);
            }
            for variable in [
                "SSL_CERT_FILE",
                "REQUESTS_CA_BUNDLE",
                "GRPC_DEFAULT_SSL_ROOTS_FILE_PATH",
                // httplib2 uses its own CA locator, not SSL_CERT_FILE.
                "HTTPLIB2_CA_CERTS",
            ] {
                command.env(variable, ca);
            }
            // env_clear also excludes inherited NO_PROXY/no_proxy and API keys.
            // Never disable certificate verification or introduce proxy bypasses.
        }
        let child = command
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Classify bounded diagnostics, never echo or retain raw lines.
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let group = child.id().and_then(|id| i32::try_from(id).ok());
        let mut agent = Self {
            child,
            group,
            diagnostics: Arc::new(AtomicU32::new(0)),
            diagnostic_reader: None,
        };
        if let Some(stderr) = agent.child.stderr.take() {
            let diagnostics = Arc::clone(&agent.diagnostics);
            agent.diagnostic_reader = Some(tokio::spawn(async move {
                let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(16 * 1024));
                while let Some(Ok(line)) = lines.next().await {
                    diagnostics.fetch_or(classify_diagnostics(&line), Ordering::Relaxed);
                }
            }));
        }
        Ok(agent)
    }

    /// Categories are observations, not terminal outcomes. No log text,
    /// authentication response material or URLs leave the subprocess reader.
    pub fn diagnostic_categories(&self) -> Vec<&'static str> {
        category_names(self.diagnostics.load(Ordering::Relaxed))
    }

    fn kill_group(&mut self) -> io::Result<()> {
        if let Some(group) = self.group {
            // SAFETY: the unreaped child leads its own group, established at
            // spawn. No signal targets the caller's process group.
            if unsafe { libc::killpg(group, libc::SIGKILL) } == -1 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error);
                }
            }
            self.group = None;
        }
        Ok(())
    }

    pub async fn stop(mut self) -> io::Result<()> {
        let group = self.group;
        self.kill_group()?;
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(io::Error::other)??;
        if let Some(group) = group {
            tokio::time::timeout(Duration::from_secs(5), async {
                while process_group_exists(group)? {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok::<_, io::Error>(())
            })
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "agent process group remains")
            })??;
        }
        Ok(())
    }
}

fn process_group_exists(group: i32) -> io::Result<bool> {
    // SAFETY: signal zero checks existence only. The group ID came from our
    // supervised child, and this never sends a signal to a reused group.
    if unsafe { libc::killpg(group, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        // A denied existence probe is not proof that the group is gone.
        // Keep waiting within stop's deadline; only ESRCH confirms shutdown.
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.kill_group();
        if let Some(reader) = &self.diagnostic_reader {
            reader.abort();
        }
    }
}

// The returned values are fixed labels, never excerpts from protocol errors.
#[allow(dead_code)]
pub fn diagnostic_categories_for_text(text: &str) -> Vec<&'static str> {
    category_names(classify_diagnostics(text))
}

fn category_names(observed: u32) -> Vec<&'static str> {
    DIAGNOSTICS
        .iter()
        .enumerate()
        .filter_map(|(index, (name, _))| (observed & (1 << index) != 0).then_some(*name))
        .collect()
}

fn classify_diagnostics(line: &str) -> u32 {
    let lower = line.to_ascii_lowercase();
    DIAGNOSTICS
        .iter()
        .enumerate()
        .fold(0, |bits, (index, (_, patterns))| {
            if patterns.iter().any(|pattern| lower.contains(pattern)) {
                bits | (1 << index)
            } else {
                bits
            }
        })
}

const DIAGNOSTICS: &[(&str, &[&str])] = &[
    (
        "mcp_bridge_environment_missing",
        &[
            "missing chaos_clamp_mcp_socket",
            "missing chaos_clamp_mcp_token",
        ],
    ),
    ("harness_activity", &["localharness", "local harness"]),
    (
        "runtime_startup_error",
        &[
            "no such file",
            "not found",
            "failed to spawn",
            "cannot execute",
        ],
    ),
    (
        "authentication_error",
        &[
            "unauthenticated",
            "invalid_grant",
            "authentication required",
        ],
    ),
    (
        "permission_error",
        &[
            "permission denied",
            "permission_denied",
            "not authorized",
            "operation not permitted",
            "permissionerror",
            "read-only file system",
        ],
    ),
    (
        "network_or_timeout",
        &[
            "connection refused",
            "econnrefused",
            "connection reset",
            "econnreset",
            "timed out",
            "deadline exceeded",
            "deadline_exceeded",
            "dns resolution failed",
            "name or service not known",
        ],
    ),
    (
        "connection_refused",
        &["connection refused", "econnrefused"],
    ),
    ("connection_reset", &["connection reset", "econnreset"]),
    (
        "deadline_exceeded",
        &["deadline exceeded", "deadline_exceeded"],
    ),
    ("operation_timed_out", &["timed out"]),
    (
        "dns_resolution_failed",
        &["dns resolution failed", "name or service not known"],
    ),
    (
        "loopback_endpoint_mentioned",
        &["127.0.0.1", "[::1]", "localhost"],
    ),
    ("tls_diagnostic", &["certificate", "ssl error", "tls error"]),
    (
        "certificate_verification_failed",
        &["certificate_verify_failed", "certificate verify failed"],
    ),
    (
        "certificate_issuer_untrusted",
        &[
            "unable to get local issuer",
            "self signed certificate",
            "self-signed certificate",
            "unknown ca",
        ],
    ),
    (
        "certificate_expired",
        &["certificate has expired", "certificate expired"],
    ),
    (
        "proxy_error",
        &["proxyerror", "proxy error", "proxy connect aborted"],
    ),
    (
        "harness_startup_eof",
        &[
            "failed to read length from stdout",
            "harness process exited",
        ],
    ),
    (
        "websocket_diagnostic",
        &["websocket", "web socket", "ws://"],
    ),
    (
        "bind_diagnostic",
        &[
            "failed to bind",
            "bind:",
            "listen tcp",
            "address already in use",
        ],
    ),
];

#[cfg(test)]
mod diagnostic_tests {
    use super::*;

    #[test]
    fn classifies_only_fixed_categories_without_retaining_log_values() {
        assert!(DIAGNOSTICS.len() <= u32::BITS as usize);
        assert_eq!(classify_diagnostics("unrecognized private text"), 0);
        let bits = classify_diagnostics(
            "SSL error: CERTIFICATE_VERIFY_FAILED: unable to get local issuer certificate",
        );
        let names: Vec<_> = DIAGNOSTICS
            .iter()
            .enumerate()
            .filter_map(|(index, (name, _))| (bits & (1 << index) != 0).then_some(*name))
            .collect();
        assert_eq!(
            names,
            [
                "tls_diagnostic",
                "certificate_verification_failed",
                "certificate_issuer_untrusted",
            ]
        );
    }

    #[test]
    fn network_diagnostics_preserve_subtype_without_exposing_endpoint() {
        assert_eq!(
            diagnostic_categories_for_text("RPC connection refused at 127.0.0.1:12345"),
            [
                "network_or_timeout",
                "connection_refused",
                "loopback_endpoint_mentioned",
            ]
        );
        assert_eq!(
            diagnostic_categories_for_text("rpc status=DEADLINE_EXCEEDED"),
            ["network_or_timeout", "deadline_exceeded"]
        );
        assert_eq!(
            diagnostic_categories_for_text("DNS resolution failed for a private endpoint"),
            ["network_or_timeout", "dns_resolution_failed"]
        );
    }

    #[tokio::test]
    async fn shutdown_confirms_entire_owned_group_has_exited() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let home = tempfile::tempdir().expect("private fixture");
        let mut agent = Agent::spawn(Path::new("/bin/sh"), home.path()).expect("fixture process");
        agent
            .child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(b"/bin/sleep 60 &\necho READY\nwait\n")
            .await
            .expect("start group descendant");
        let stdout = agent.child.stdout.take().expect("stdout");
        let mut lines = BufReader::new(stdout).lines();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), lines.next_line())
                .await
                .expect("fixture ready")
                .expect("fixture stdout")
                .as_deref(),
            Some("READY")
        );
        let group = agent.group.expect("owned group");
        agent
            .stop()
            .await
            .expect("root reaped and whole group gone");
        assert!(!process_group_exists(group).expect("existence check"));
    }
}
