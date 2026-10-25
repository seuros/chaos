use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use super::{CommandShell, ConfiguredHandler};

/// Per-stream cap on what a hook may write. An oversize stream fails the hook
/// rather than being truncated, so this must comfortably exceed any legitimate
/// `additionalContext`: a session-start hook that hands a resident its identity
/// and recent journals can run well past 128 KiB, and at that size the whole
/// wake was being dropped on the floor.
const OUTPUT_LIMIT: u64 = 1024 * 1024;
const OUTPUT_LIMIT_LABEL: &str = "1 MiB";

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

async fn read_output(stream: impl AsyncRead + Unpin) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    stream
        .take(OUTPUT_LIMIT + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return Err(std::io::Error::other(format!(
            "hook output exceeds {OUTPUT_LIMIT_LABEL}"
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
            stdin.write_all(input_json.as_bytes()).await?;
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            write,
            read_output(stdout),
            read_output(stderr),
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
