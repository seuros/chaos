use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::error::GuestError;
use crate::protocol::JsonRpcMessage;
use crate::transport::{MessageTransport, TransportFuture};

pub struct StdioChild {
    pub child: Child,
    pub stdout: BufReader<ChildStdout>,
    pub stdin: BufWriter<ChildStdin>,
}

impl StdioChild {
    pub fn spawn(
        command: &str,
        args: &[String],
        env: &HashMap<String, String>,
        cwd: Option<&Path>,
    ) -> Result<Self, GuestError> {
        let mut child = tokio::process::Command::new(command);
        child
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        if let Some(cwd) = cwd {
            child.current_dir(cwd);
        }

        for (key, value) in env {
            child.env(key, value);
        }

        let mut child = child.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| GuestError::Protocol("missing child stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GuestError::Protocol("missing child stdout".to_string()))?;

        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
            stdin: BufWriter::new(stdin),
        })
    }
}

pub struct StdioTransport {
    reader: Mutex<BufReader<ChildStdout>>,
    writer: Mutex<BufWriter<ChildStdin>>,
    child: Mutex<Child>,
    shutdown_timeout: Duration,
    kill_timeout: Duration,
    closed: AtomicBool,
}

impl StdioTransport {
    pub fn new(child: StdioChild, shutdown_timeout: Duration, kill_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            reader: Mutex::new(child.stdout),
            writer: Mutex::new(child.stdin),
            child: Mutex::new(child.child),
            shutdown_timeout,
            kill_timeout,
            closed: AtomicBool::new(false),
        })
    }

    async fn write_message(&self, message: &JsonRpcMessage) -> Result<(), GuestError> {
        let json = serde_json::to_string(message)?;
        let mut writer = self.writer.lock().await;
        writer.write_all(json.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok(())
    }
}

impl MessageTransport for StdioTransport {
    fn send<'a>(&'a self, message: JsonRpcMessage) -> TransportFuture<'a, ()> {
        Box::pin(async move { self.write_message(&message).await })
    }

    fn recv<'a>(&'a self) -> TransportFuture<'a, JsonRpcMessage> {
        Box::pin(async move {
            loop {
                if self.closed.load(Ordering::Relaxed) {
                    return Err(GuestError::Disconnected);
                }

                let mut line = String::new();
                let bytes = {
                    let mut reader = self.reader.lock().await;
                    reader.read_line(&mut line).await?
                };

                if bytes == 0 {
                    self.closed.store(true, Ordering::Relaxed);
                    return Err(GuestError::Disconnected);
                }

                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                return serde_json::from_str(trimmed).map_err(GuestError::from);
            }
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async move {
            if self.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }

            {
                let mut writer = self.writer.lock().await;
                let _ = writer.flush().await;
                let _ = writer.shutdown().await;
            }

            let mut child = self.child.lock().await;
            if timeout(self.shutdown_timeout, child.wait()).await.is_ok() {
                return Ok(());
            }

            let _ = child.start_kill();
            let _ = timeout(self.kill_timeout, child.wait()).await;
            Ok(())
        })
    }
}

#[derive(Debug, Clone)]
pub struct StdioProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub shutdown_timeout: Duration,
    pub kill_timeout: Duration,
}
