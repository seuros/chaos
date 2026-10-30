use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use super::{CommandShell, ConfiguredHandler};

/// Default per-stream cap on what a hook may write, in bytes. Operators raise it
/// with the `hook_output_limit_kib` config key; the dtrace side receives it on
/// `CommandShell::output_limit_bytes`.
pub(crate) const DEFAULT_OUTPUT_LIMIT_BYTES: u64 = 128 * 1024;

/// Human label for a byte limit: whole KiB when it divides evenly, bytes otherwise.
pub(crate) fn output_limit_label(bytes: u64) -> String {
    if bytes > 0 && bytes.is_multiple_of(1024) {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} bytes")
    }
}

#[derive(Debug)]
pub(crate) struct CommandRunResult {
    pub started_at: i64,
    pub completed_at: i64,
    pub duration_ms: i64,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub error: Option<String>,
}

struct HookChild(Child, Option<u32>);
impl Drop for HookChild {
    fn drop(&mut self) {
        if let Some(group) = self.1 {
            let _ = chaos_pty::process_group::kill_process_group(group);
        }
    }
}

async fn read_output(stream: impl AsyncRead + Unpin, limit: u64) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    stream
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other(format!(
            "hook output exceeds {}",
            output_limit_label(limit)
        )));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) async fn run_command(
    shell: &CommandShell,
    handler: &ConfiguredHandler,
    input_json: &str,
    cwd: &Path,
) -> CommandRunResult {
    let started_at = jiff::Timestamp::now().as_second();
    let started = Instant::now();
    let operation = async {
        let mut command = build_command(shell, handler, cwd)?;
        command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let child = command.spawn()?;
        let group = child.id();
        let mut child = HookChild(child, group);
        let mut stdin = child
            .0
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing hook stdin"))?;
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing hook stdout"))?;
        let stderr = child
            .0
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("missing hook stderr"))?;
        // Drain outputs while writing input, with one timeout covering every pipe.
        let write = async {
            match stdin.write_all(input_json.as_bytes()).await {
                Err(error) if error.kind() != std::io::ErrorKind::BrokenPipe => return Err(error),
                _ => {}
            }
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            write,
            read_output(stdout, shell.output_limit_bytes),
            read_output(stderr, shell.output_limit_bytes),
            child.0.wait(),
        )?;
        Ok::<_, anyhow::Error>((stdout, stderr, status.code()))
    };
    let result = tokio::time::timeout(Duration::from_secs(handler.timeout_sec), operation).await;
    let (stdout, stderr, exit_code, error) = match result {
        Ok(Ok((stdout, stderr, code))) => (stdout, stderr, code, None),
        Ok(Err(error)) => (String::new(), String::new(), None, Some(error.to_string())),
        Err(_) => (
            String::new(),
            String::new(),
            None,
            Some(format!("hook timed out after {}s", handler.timeout_sec)),
        ),
    };
    CommandRunResult {
        started_at,
        completed_at: jiff::Timestamp::now().as_second(),
        duration_ms: started.elapsed().as_millis().try_into().unwrap_or(i64::MAX),
        exit_code,
        stdout,
        stderr,
        error,
    }
}

fn build_command(
    shell: &CommandShell,
    handler: &ConfiguredHandler,
    cwd: &Path,
) -> anyhow::Result<Command> {
    let mut argv = if shell.program.is_empty() {
        vec!["/bin/sh".into(), "-c".into()]
    } else {
        let mut argv = vec![shell.program.clone()];
        argv.extend(shell.args.clone());
        argv
    };
    argv.push(handler.command.clone());
    if let Some(builder) = &shell.builder {
        return builder(argv, cwd);
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    Ok(command)
}

#[cfg(test)]
mod tests;
