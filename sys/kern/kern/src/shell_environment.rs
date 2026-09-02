//! CWD-aware user shell environment capture.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use chaos_ipc::ProcessId;
use chaos_snitch::SessionTelemetry;
use chaos_traits::router::Adapter;
use chaos_traits::router::AdapterError;
use chaos_traits::router::DEFAULT_ADAPTER_CAPACITY;
use tokio::fs;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::time::timeout;
use tracing::Instrument;
use tracing::info_span;

use crate::shell::Shell;
use crate::shell::ShellType;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEnvironment {
    pub(crate) vars: HashMap<String, String>,
    pub(crate) cwd: PathBuf,
}

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10);
const LEGACY_SNAPSHOT_DIR: &str = "shell_snapshots";
const EXCLUDED_VARS: &[&str] = &["PWD", "OLDPWD"];

enum ShellEnvironmentOp {
    Refresh { cwd: PathBuf },
}

pub(crate) enum ShellEnvironmentStartup {
    Inherited(Arc<ShellEnvironment>),
    Capture(PathBuf),
    #[cfg(test)]
    Idle,
}

#[derive(Clone)]
pub(crate) struct ShellEnvironmentActor {
    mailbox: Adapter<ShellEnvironmentOp, ()>,
}

impl ShellEnvironmentActor {
    pub(crate) fn spawn(
        chaos_home: PathBuf,
        session_id: ProcessId,
        startup: ShellEnvironmentStartup,
        shell: &mut Shell,
        session_telemetry: SessionTelemetry,
    ) -> Self {
        Self::spawn_inner(
            chaos_home,
            session_id,
            startup,
            shell,
            Some(session_telemetry),
        )
    }

    fn spawn_inner(
        chaos_home: PathBuf,
        session_id: ProcessId,
        startup: ShellEnvironmentStartup,
        shell: &mut Shell,
        session_telemetry: Option<SessionTelemetry>,
    ) -> Self {
        let (initial_environment, initial_cwd) = match startup {
            ShellEnvironmentStartup::Inherited(environment) => (Some(environment), None),
            ShellEnvironmentStartup::Capture(cwd) => (None, Some(cwd)),
            #[cfg(test)]
            ShellEnvironmentStartup::Idle => (None, None),
        };

        let (environment_tx, environment_rx) = watch::channel(initial_environment);
        shell.shell_environment = environment_rx;
        let capture_shell = shell.clone();
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);

        tokio::spawn(async move {
            if let Err(err) = remove_legacy_snapshot_storage(&chaos_home).await {
                tracing::warn!("Failed to remove legacy shell snapshot storage: {err:?}");
            }

            if let Some(initial_cwd) = initial_cwd {
                capture_and_publish_environment(
                    session_id,
                    initial_cwd.as_path(),
                    &capture_shell,
                    &environment_tx,
                    session_telemetry.as_ref(),
                )
                .await;
            }

            while let Some(packet) = receiver.recv().await {
                let ShellEnvironmentOp::Refresh { mut cwd } = packet.op;
                let mut replies = packet.reply.into_iter().collect::<Vec<_>>();

                while let Ok(queued) = receiver.try_recv() {
                    let ShellEnvironmentOp::Refresh { cwd: queued_cwd } = queued.op;
                    cwd = queued_cwd;
                    replies.extend(queued.reply);
                }

                capture_and_publish_environment(
                    session_id,
                    cwd.as_path(),
                    &capture_shell,
                    &environment_tx,
                    session_telemetry.as_ref(),
                )
                .await;

                for reply in replies {
                    let _ = reply.send(());
                }
            }
        });

        Self { mailbox }
    }

    pub(crate) async fn refresh(&self, cwd: PathBuf) -> Result<(), AdapterError> {
        self.mailbox.send(ShellEnvironmentOp::Refresh { cwd }).await
    }

    #[cfg(test)]
    async fn refresh_and_wait(&self, cwd: PathBuf) -> Result<(), AdapterError> {
        self.mailbox.call(ShellEnvironmentOp::Refresh { cwd }).await
    }

    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        let (mailbox, mut receiver) = Adapter::bounded(DEFAULT_ADAPTER_CAPACITY);
        tokio::spawn(async move {
            while let Some(packet) = receiver.recv().await {
                if let Some(reply) = packet.reply {
                    let _ = reply.send(());
                }
            }
        });
        Self { mailbox }
    }
}

async fn capture_and_publish_environment(
    session_id: ProcessId,
    session_cwd: &Path,
    shell: &Shell,
    environment_tx: &watch::Sender<Option<Arc<ShellEnvironment>>>,
    session_telemetry: Option<&SessionTelemetry>,
) {
    let timer = session_telemetry
        .map(|telemetry| telemetry.start_timer("chaos.shell_environment.duration_ms", &[]));
    let environment = ShellEnvironment::try_new(session_cwd, shell)
        .instrument(info_span!(
            "shell_environment_capture",
            process_id = %session_id
        ))
        .await
        .map(Arc::new);
    let success = environment.is_ok();
    let success_tag = if success { "true" } else { "false" };
    if let Some(timer) = timer {
        let _ = timer.map(|timer| timer.record(&[("success", success_tag)]));
    }
    if let Some(session_telemetry) = session_telemetry {
        let mut counter_tags = vec![("success", success_tag)];
        if let Some(failure_reason) = environment.as_ref().err() {
            counter_tags.push(("failure_reason", *failure_reason));
        }
        session_telemetry.counter("chaos.shell_environment", 1, &counter_tags);
    }

    if let Ok(environment) = environment {
        drop(environment_tx.send_replace(Some(environment)));
    }
}

impl ShellEnvironment {
    async fn try_new(session_cwd: &Path, shell: &Shell) -> std::result::Result<Self, &'static str> {
        let vars = match capture_shell_environment(shell, session_cwd).await {
            Ok(vars) => vars,
            Err(err) => {
                tracing::warn!(
                    "Failed to capture environment for {}: {err:?}",
                    shell.name()
                );
                return Err("capture_failed");
            }
        };

        tracing::debug!(
            variable_count = vars.len(),
            "User shell environment captured"
        );
        Ok(Self {
            vars,
            cwd: session_cwd.to_path_buf(),
        })
    }
}

async fn capture_shell_environment(shell: &Shell, cwd: &Path) -> Result<HashMap<String, String>> {
    let marker_id = ProcessId::new().to_string();
    let marker = format!("\0chaos-shell-environment:{marker_id}\0");
    let script = environment_capture_script(shell.shell_type.clone(), &marker_id);
    let output = run_script_with_timeout(shell, &script, CAPTURE_TIMEOUT, true, cwd).await?;
    parse_environment_output(&output, marker.as_bytes())
}

fn environment_capture_script(shell_type: ShellType, marker_id: &str) -> String {
    let startup = match shell_type {
        ShellType::Zsh => {
            r#"if [[ -n "$ZDOTDIR" ]]; then
  rc="$ZDOTDIR/.zshrc"
else
  rc="$HOME/.zshrc"
fi
[[ -r "$rc" ]] && . "$rc"
"#
        }
        ShellType::Bash => {
            r#"if [ -z "$BASH_ENV" ] && [ -r "$HOME/.bashrc" ]; then
  . "$HOME/.bashrc"
fi
"#
        }
        ShellType::Sh => {
            r#"if [ -n "$ENV" ] && [ -r "$ENV" ]; then
  . "$ENV"
fi
"#
        }
    };

    format!("{startup}printf '\\0chaos-shell-environment:%s\\0' '{marker_id}'\n/usr/bin/env -0\n")
}

fn parse_environment_output(output: &[u8], marker: &[u8]) -> Result<HashMap<String, String>> {
    let marker_start = output
        .windows(marker.len())
        .rposition(|window| window == marker)
        .context("shell environment output missing marker")?;
    let environment = &output[marker_start + marker.len()..];
    let mut vars = HashMap::new();
    let mut skipped_non_utf8 = 0usize;

    for entry in environment.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        if separator == 0 {
            continue;
        }
        let Ok(name) = std::str::from_utf8(&entry[..separator]) else {
            skipped_non_utf8 += 1;
            continue;
        };
        let Ok(value) = std::str::from_utf8(&entry[separator + 1..]) else {
            skipped_non_utf8 += 1;
            continue;
        };
        if EXCLUDED_VARS.contains(&name) {
            continue;
        }
        vars.insert(name.to_string(), value.to_string());
    }

    if skipped_non_utf8 > 0 {
        tracing::warn!(
            skipped_variable_count = skipped_non_utf8,
            "Skipped non-UTF-8 variables while capturing the shell environment"
        );
    }
    if vars.is_empty() {
        bail!("captured shell environment was empty");
    }

    Ok(vars)
}

async fn run_script_with_timeout(
    shell: &Shell,
    script: &str,
    capture_timeout: Duration,
    use_login_shell: bool,
    cwd: &Path,
) -> Result<Vec<u8>> {
    let args = shell.derive_exec_args(script, use_login_shell);
    let shell_name = shell.name();

    let mut handler = Command::new(&args[0]);
    handler.args(&args[1..]);
    handler.stdin(Stdio::null());
    handler.current_dir(cwd);
    unsafe {
        handler.pre_exec(|| {
            chaos_pty::process_group::detach_from_tty()?;
            Ok(())
        });
    }
    handler.kill_on_drop(true);
    let output = timeout(capture_timeout, handler.output())
        .await
        .map_err(|_| anyhow!("Environment capture timed out for {shell_name}"))?
        .with_context(|| format!("Failed to execute {shell_name}"))?;

    if !output.status.success() {
        let status = output.status;
        bail!("Environment capture exited with status {status}");
    }

    Ok(output.stdout)
}

async fn remove_legacy_snapshot_storage(chaos_home: &Path) -> Result<()> {
    let path = chaos_home.join(LEGACY_SNAPSHOT_DIR);
    let metadata = match fs::symlink_metadata(&path).await {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path).await?;
    } else if metadata.is_dir() {
        fs::remove_dir_all(path).await?;
    }

    Ok(())
}

#[cfg(test)]
#[path = "shell_environment_tests.rs"]
mod tests;
