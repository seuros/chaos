//! Best-effort, process-scoped macOS sandbox audit capture.

use std::collections::HashSet;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::task::JoinHandle;

use super::head_tail_buffer::HeadTailBuffer;
use pid_tracker::PidTracker;

mod pid_tracker;
#[cfg(test)]
mod tests;

const AUDIT_LOG_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxDenial {
    pub name: String,
    pub capability: String,
}

#[derive(Debug)]
pub struct DenialLogger {
    log_stream: Child,
    root_pid: Option<i32>,
    pid_tracker: Option<PidTracker>,
    log_reader: Option<JoinHandle<HeadTailBuffer>>,
}

impl DenialLogger {
    /// Start before spawning the sandboxed command. Audit access is best-effort.
    pub fn new() -> Option<Self> {
        Self::from_log_stream(start_log_stream()?)
    }

    fn from_log_stream(mut log_stream: Child) -> Option<Self> {
        let stdout = log_stream.stdout.take()?;
        let log_reader = tokio::spawn(async move {
            let mut reader = stdout;
            let mut logs = HeadTailBuffer::new(AUDIT_LOG_MAX_BYTES);
            let mut chunk = [0; 8192];
            loop {
                match reader.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(count) => logs.push_chunk(chunk[..count].to_vec()),
                    Err(error) => {
                        tracing::debug!("sandbox audit stream unavailable: {error}");
                        break;
                    }
                }
            }
            logs
        });

        Some(Self {
            log_stream,
            root_pid: None,
            pid_tracker: None,
            log_reader: Some(log_reader),
        })
    }

    pub fn on_child_spawn(&mut self, child: &Child) {
        if let Some(pid) = child.id() {
            self.track_pid(pid);
        }
    }

    pub(crate) fn track_pid(&mut self, pid: u32) {
        if let Ok(pid) = i32::try_from(pid)
            && pid > 0
        {
            self.root_pid = Some(pid);
            self.pid_tracker = PidTracker::new(pid);
        }
    }

    pub async fn finish(mut self) -> Vec<SandboxDenial> {
        // Unified logging can deliver the last denial after the command exits.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut pid_set = match self.pid_tracker.take() {
            Some(tracker) => tracker.stop().await,
            None => Default::default(),
        };
        if let Some(pid) = self.root_pid {
            pid_set.insert(pid);
        }

        if pid_set.is_empty() {
            return Vec::new();
        }

        if let Err(error) = self.log_stream.start_kill() {
            tracing::debug!("failed to stop sandbox audit stream: {error}");
        }

        let logs = match self.log_reader.as_mut() {
            Some(handle) => {
                match tokio::time::timeout(Duration::from_millis(500), &mut *handle).await {
                    Ok(Ok(logs)) => logs,
                    result => {
                        handle.abort();
                        tracing::debug!("failed to drain sandbox audit stream: {result:?}");
                        return Vec::new();
                    }
                }
            }
            None => return Vec::new(),
        };
        collect_buffered_denials(&logs, &pid_set)
    }
}

fn collect_buffered_denials(logs: &HeadTailBuffer, pid_set: &HashSet<i32>) -> Vec<SandboxDenial> {
    let mut bytes = logs.to_bytes();
    if logs.omitted_bytes() > 0 {
        // Never splice two partial JSON records into fabricated audit evidence.
        bytes.insert(AUDIT_LOG_MAX_BYTES / 2, b'\n');
    }
    collect_denials(&String::from_utf8_lossy(&bytes), pid_set)
}

impl Drop for DenialLogger {
    fn drop(&mut self) {
        if let Err(error) = self.log_stream.start_kill() {
            tracing::debug!("failed to stop sandbox audit stream: {error}");
        }
        if let Some(handle) = self.log_reader.take() {
            handle.abort();
        }
    }
}

fn collect_denials(logs: &str, pid_set: &HashSet<i32>) -> Vec<SandboxDenial> {
    let mut seen = HashSet::new();
    let mut denials = Vec::new();
    // Prefer the last failures over earlier, possibly harmless capability probes.
    for line in logs.lines().rev() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line)
            && let Some(msg) = json.get("eventMessage").and_then(|v| v.as_str())
            && let Some((pid, name, capability)) = parse_message(msg)
            && pid_set.contains(&pid)
            && seen.insert((name.clone(), capability.clone()))
        {
            denials.push(SandboxDenial { name, capability });
            if denials.len() == 16 {
                break;
            }
        }
    }
    denials.reverse();
    denials
}

fn start_log_stream() -> Option<Child> {
    use std::process::Stdio;

    const PREDICATE: &str = concat!(
        r#"(((processID == 0) AND (senderImagePath CONTAINS "/Sandbox")) "#,
        r#"OR (subsystem == "com.apple.sandbox.reporting"))"#
    );

    let result = tokio::process::Command::new("/usr/bin/log")
        .args(["stream", "--style", "ndjson", "--predicate", PREDICATE])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    match result {
        Ok(child) => Some(child),
        Err(error) => {
            tracing::debug!("cannot capture sandbox audit stream: {error}");
            None
        }
    }
}

fn parse_message(msg: &str) -> Option<(i32, String, String)> {
    // Example message:
    // Sandbox: processname(1234) deny(1) capability-name args...
    let (process, denial) = msg
        .strip_prefix("Sandbox:")?
        .trim_start()
        .split_once(" deny(")?;
    let (name, pid_str) = process.strip_suffix(')')?.rsplit_once('(')?;
    let (decision, capability) = denial.split_once(')')?;
    decision.parse::<u32>().ok()?;
    let pid = pid_str.parse::<i32>().ok()?;
    let capability = capability.trim_start();
    if pid <= 0 || name.is_empty() || capability.is_empty() {
        return None;
    }
    Some((pid, name.to_string(), capability.to_string()))
}
