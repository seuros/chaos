use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::io::BufWriter;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::ChildStdout;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::error::GuestError;
use crate::protocol::JsonRpcMessage;
use crate::transport::MessageTransport;
use crate::transport::TransportFuture;

const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
const MAX_RETAINED_BUFFER_BYTES: usize = 64 * 1024;

// The caller retains `buf` across cancelled reads. No second UTF-8 buffer is
// needed, and a select! losing to a command cannot discard a partial frame.
async fn read_line_bounded<R>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_len: usize,
) -> Result<bool, GuestError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!buf.is_empty());
        }
        let newline = available.iter().position(|&byte| byte == b'\n');
        let len = newline.unwrap_or(available.len());
        if len > max_len.saturating_sub(buf.len()) {
            return Err(GuestError::Protocol(format!(
                "stdio line exceeded {max_len} bytes"
            )));
        }
        buf.extend_from_slice(&available[..len]);
        reader.consume(len + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(true);
        }
    }
}

fn recycle_buffer(buf: &mut Vec<u8>) {
    if buf.capacity() > MAX_RETAINED_BUFFER_BYTES {
        *buf = Vec::new();
    } else {
        buf.clear();
    }
}

struct RecycledBuffer<'a>(&'a mut Vec<u8>);

impl Drop for RecycledBuffer<'_> {
    fn drop(&mut self) {
        recycle_buffer(self.0);
    }
}

struct StdioReader {
    reader: BufReader<ChildStdout>,
    buffer: Vec<u8>,
}

struct StdioWriter {
    writer: BufWriter<ChildStdin>,
    buffer: Vec<u8>,
}

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
    reader: Mutex<StdioReader>,
    writer: Mutex<StdioWriter>,
    child: Mutex<Child>,
    write_timeout: Duration,
    shutdown_timeout: Duration,
    kill_timeout: Duration,
    closed: AtomicBool,
    reaped: AtomicBool,
    shutdown_lock: Mutex<()>,
}

impl StdioTransport {
    pub fn new(
        child: StdioChild,
        write_timeout: Duration,
        shutdown_timeout: Duration,
        kill_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            reader: Mutex::new(StdioReader {
                reader: child.stdout,
                buffer: Vec::new(),
            }),
            writer: Mutex::new(StdioWriter {
                writer: child.stdin,
                buffer: Vec::new(),
            }),
            child: Mutex::new(child.child),
            write_timeout,
            shutdown_timeout,
            kill_timeout,
            closed: AtomicBool::new(false),
            reaped: AtomicBool::new(false),
            shutdown_lock: Mutex::new(()),
        })
    }

    async fn write_message(&self, message: &JsonRpcMessage) -> Result<(), GuestError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(GuestError::Disconnected);
        }
        let result = timeout(self.write_timeout, async {
            let mut state = self.writer.lock().await;
            let StdioWriter { writer, buffer } = &mut *state;
            let buffer = RecycledBuffer(buffer);
            buffer.0.clear();
            serde_json::to_writer(&mut *buffer.0, message)?;
            buffer.0.push(b'\n');
            let result = async {
                writer.write_all(buffer.0).await?;
                writer.flush().await?;
                Ok::<(), std::io::Error>(())
            }
            .await;
            result.map_err(GuestError::from)
        })
        .await;
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                self.closed.store(true, Ordering::Release);
                Err(error)
            }
            Err(_) => {
                self.closed.store(true, Ordering::Release);
                Err(GuestError::Timeout(self.write_timeout))
            }
        }
    }

    async fn reap_child(&self, graceful_timeout: Option<Duration>) -> Result<(), GuestError> {
        let mut child = self.child.lock().await;
        let pid = child.id();

        if child.try_wait()?.is_some() {
            self.reaped.store(true, Ordering::Release);
            return Ok(());
        }

        if let Some(graceful_timeout) = graceful_timeout {
            match timeout(graceful_timeout, child.wait()).await {
                Ok(Ok(status)) => {
                    self.reaped.store(true, Ordering::Release);
                    tracing::debug!(
                        ?pid,
                        ?status,
                        "MCP stdio child exited during graceful shutdown"
                    );
                    return Ok(());
                }
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => {
                    tracing::warn!(
                        ?pid,
                        ?graceful_timeout,
                        "MCP stdio child ignored graceful shutdown; forcing termination"
                    );
                }
            }
        }

        if let Err(error) = child.start_kill() {
            if child.try_wait()?.is_some() {
                self.reaped.store(true, Ordering::Release);
                return Ok(());
            }
            return Err(error.into());
        }

        match timeout(self.kill_timeout, child.wait()).await {
            Ok(Ok(status)) => {
                self.reaped.store(true, Ordering::Release);
                tracing::debug!(?pid, ?status, "MCP stdio child was killed and reaped");
                Ok(())
            }
            Ok(Err(error)) => Err(error.into()),
            Err(_) => {
                tracing::error!(
                    ?pid,
                    kill_timeout = ?self.kill_timeout,
                    "timed out reaping killed MCP stdio child"
                );
                Err(GuestError::Timeout(self.kill_timeout))
            }
        }
    }

    async fn shutdown_inner(&self, graceful: bool) -> Result<(), GuestError> {
        let _shutdown_guard = self.shutdown_lock.lock().await;
        if self.reaped.load(Ordering::Acquire) {
            return Ok(());
        }
        self.closed.store(true, Ordering::Release);

        if graceful {
            match timeout(self.shutdown_timeout, async {
                let mut writer = self.writer.lock().await;
                writer.writer.flush().await?;
                writer.writer.shutdown().await?;
                Ok::<(), std::io::Error>(())
            })
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::warn!(%error, "failed to close MCP stdio input; forcing child shutdown");
                }
                Err(_) => {
                    tracing::warn!(
                        shutdown_timeout = ?self.shutdown_timeout,
                        "timed out closing MCP stdio input; forcing child shutdown"
                    );
                }
            }
        }

        self.reap_child(graceful.then_some(self.shutdown_timeout))
            .await
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

                let mut state = self.reader.lock().await;
                let StdioReader { reader, buffer } = &mut *state;
                match read_line_bounded(reader, buffer, MAX_LINE_BYTES).await {
                    Ok(true) => {}
                    Ok(false) => {
                        recycle_buffer(buffer);
                        self.closed.store(true, Ordering::Relaxed);
                        return Err(GuestError::Disconnected);
                    }
                    Err(error) => {
                        recycle_buffer(buffer);
                        self.closed.store(true, Ordering::Relaxed);
                        return Err(error);
                    }
                }
                // Borrow valid UTF-8; retain the old lossy behavior for noisy
                // stdout without copying every valid JSON-RPC frame.
                let line = String::from_utf8_lossy(buffer);
                let trimmed = line.trim();
                let result = (!trimmed.is_empty()).then(|| serde_json::from_str(trimmed));
                recycle_buffer(buffer);
                match result {
                    Some(Ok(message)) => return Ok(message),
                    Some(Err(error)) => {
                        tracing::warn!(%error, "skipping malformed line on MCP stdio stream");
                    }
                    None => {}
                }
            }
        })
    }

    fn shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async move { self.shutdown_inner(true).await })
    }

    fn force_shutdown<'a>(&'a self) -> TransportFuture<'a, ()> {
        Box::pin(async move { self.shutdown_inner(false).await })
    }
}

#[derive(Debug, Clone)]
pub struct StdioProcessConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub write_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub kill_timeout: Duration,
}

#[cfg(all(test, unix))]
mod tests;
