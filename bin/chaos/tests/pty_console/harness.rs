use anyhow::{Context, Result, ensure};
use chaos_kern::config::set_project_trust_level;
use chaos_kern::user_settings::BootstrapConfig;
use chaos_pty::{ProcessHandle, SpawnedProcess, TerminalSize};
use chaos_test_fixtures::TEST_MODEL;
use core_test_support::streaming_sse::StreamingSseServer;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(15);
const INITIAL_SIZE: TerminalSize = TerminalSize {
    rows: 38,
    cols: 100,
};
const OUTPUT_TAIL_BYTES: usize = 4 * 1024;

#[derive(Clone, Default)]
struct TerminalSnapshot {
    screen: String,
    output_tail: String,
    cursor_hidden: bool,
    alternate_screen: bool,
    eof: bool,
    error: Option<String>,
}

impl TerminalSnapshot {
    fn diagnostic(&self) -> String {
        format!(
            "error: {:?}\nEOF: {}\nscreen:\n{}\nrecent terminal bytes:\n{}",
            self.error,
            self.eof,
            self.screen,
            self.output_tail.replace('\x1b', "<ESC>"),
        )
    }
}

struct TerminalCapture {
    parser: vt100::Parser,
    query_tail: [u8; 4],
    output_tail: Vec<u8>,
}

impl TerminalCapture {
    fn new() -> Self {
        Self {
            parser: vt100::Parser::new(INITIAL_SIZE.rows, INITIAL_SIZE.cols, 0),
            query_tail: [0; 4],
            output_tail: Vec::new(),
        }
    }

    fn process(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        for &byte in bytes {
            // Capture the cursor at each query, including when its escape
            // sequence crosses output reads. Never feed query replies to stdout.
            self.parser.process(&[byte]);
            self.query_tail.rotate_left(1);
            self.query_tail[3] = byte;
            if self.query_tail == *b"\x1b[6n" {
                let (row, col) = self.parser.screen().cursor_position();
                replies.push(format!("\x1b[{};{}R", row + 1, col + 1).into_bytes());
            }
        }
        self.output_tail.extend_from_slice(bytes);
        let excess = self.output_tail.len().saturating_sub(OUTPUT_TAIL_BYTES);
        self.output_tail.drain(..excess);
        replies
    }

    fn snapshot(&self) -> TerminalSnapshot {
        let screen = self.parser.screen();
        TerminalSnapshot {
            screen: screen.contents(),
            output_tail: String::from_utf8_lossy(&self.output_tail).into_owned(),
            cursor_hidden: screen.hide_cursor(),
            alternate_screen: screen.alternate_screen(),
            ..Default::default()
        }
    }
}

type ResizeRequest = (TerminalSize, oneshot::Sender<()>);

/// Own every child and reader: failures cannot leave a console or daemon running.
pub(super) struct PtyConsole {
    session: ProcessHandle,
    capture: JoinHandle<()>,
    snapshots: watch::Receiver<TerminalSnapshot>,
    resize_tx: mpsc::Sender<ResizeRequest>,
    exit_rx: Option<oneshot::Receiver<i32>>,
    journal: Child,
    _root: TempDir,
}

impl PtyConsole {
    pub(super) async fn start(server: &StreamingSseServer, prompt: Option<&str>) -> Result<Self> {
        // Short paths also fit Unix-socket limits when macOS gives TMPDIR a
        // long path. All settings, credentials, caches and scripts are isolated.
        let root = tempfile::Builder::new()
            .prefix("chaos-pty-")
            .tempdir_in("/tmp")?;
        let home = root.path().join("home");
        let work = root.path().join("work");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&work)?;
        let home = home.canonicalize()?;
        let work = work.canonicalize()?;
        std::fs::write(
            home.join("config.toml"),
            toml::to_string(&BootstrapConfig {
                storage_url: Some(format!("sqlite://{}", home.join("chaos.sqlite").display())),
                egress_url: None,
            })?,
        )?;
        set_project_trust_level(&home, &work, chaos_ipc::config_types::TrustLevel::Trusted)?;
        let catalog = home.join("models.json");
        std::fs::write(
            &catalog,
            serde_json::to_vec(&serde_json::json!({
                "models": [chaos_kern::test_support::test_model_info(TEST_MODEL)]
            }))?,
        )?;

        let env = HashMap::from([
            ("HOME".into(), root.path().to_string_lossy().into_owned()),
            ("CHAOS_HOME".into(), home.to_string_lossy().into_owned()),
            (
                "XDG_CONFIG_HOME".into(),
                root.path().join("config").to_string_lossy().into_owned(),
            ),
            (
                "XDG_CACHE_HOME".into(),
                root.path().join("cache").to_string_lossy().into_owned(),
            ),
            ("TMPDIR".into(), root.path().to_string_lossy().into_owned()),
            ("SHELL".into(), "/bin/sh".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
        ]);
        let socket = home.join("run/journald.sock");
        let mut journal = Command::new(chaos_which::cargo_bin("chaos_journald")?)
            .env_clear()
            .envs(&env)
            .arg("--socket")
            .arg(&socket)
            .arg("--db")
            .arg(home.join("chaos.sqlite"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let client = chaos_journald::JournalRpcClient::new(socket);
        tokio::time::timeout(WAIT, async {
            let mut poll = tokio::time::interval(Duration::from_millis(20));
            loop {
                poll.tick().await;
                if client.hello("pty-console-test").await.is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                ensure!(
                    journal.try_wait()?.is_none(),
                    "journald exited before readiness"
                );
            }
        })
        .await
        .context("journald readiness timed out")??;

        let mut args = vec!["--no-alt-screen".to_string()];
        for setting in [
            "model_provider=\"pty-fixture\"".to_string(),
            format!("model={TEST_MODEL:?}"),
            format!(
                "model_providers.pty-fixture={{name=\"PTY Fixture\",base_url=\"{}/v1\",\
                 wire_api=\"responses\",supports_websockets=false,\
                 request_max_retries=0,stream_max_retries=0}}",
                server.uri()
            ),
            format!("model_catalog_json={:?}", catalog.to_string_lossy()),
            "cli_auth_credentials_store=\"ephemeral\"".to_string(),
            "analytics.enabled=false".to_string(),
            "machine_warnings.enabled=false".to_string(),
            "tui.animations=false".to_string(),
            "tui.notifications=false".to_string(),
        ] {
            args.extend(["-c".to_string(), setting]);
        }
        if let Some(prompt) = prompt {
            args.push(prompt.to_string());
        }
        let SpawnedProcess {
            session,
            mut stdout_rx,
            stderr_rx: _,
            exit_rx,
        } = chaos_pty::spawn_pty_process(
            env!("CARGO_BIN_EXE_chaos"),
            &args,
            &work,
            &env,
            &None,
            INITIAL_SIZE,
        )
        .await?;
        let writer = session.writer_sender();
        let (snapshot_tx, snapshots) = watch::channel(TerminalSnapshot::default());
        let (resize_tx, mut resize_rx) = mpsc::channel::<ResizeRequest>(16);
        let capture = tokio::spawn(async move {
            let mut terminal = TerminalCapture::new();
            loop {
                tokio::select! {
                    bytes = stdout_rx.recv() => {
                        let Some(bytes) = bytes else {
                            let mut snapshot = terminal.snapshot();
                            snapshot.eof = true;
                            snapshot_tx.send_replace(snapshot);
                            break;
                        };
                        for reply in terminal.process(&bytes) {
                            if writer.send(reply).await.is_err() {
                                let mut snapshot = terminal.snapshot();
                                snapshot.error = Some("PTY input closed during cursor query".into());
                                snapshot_tx.send_replace(snapshot);
                                return;
                            }
                        }
                        snapshot_tx.send_replace(terminal.snapshot());
                    }
                    resize = resize_rx.recv() => {
                        let Some((size, ack)) = resize else { break };
                        terminal.parser.screen_mut().set_size(size.rows, size.cols);
                        let _ = ack.send(());
                    }
                }
            }
        });
        let mut console = Self {
            session,
            capture,
            snapshots,
            resize_tx,
            exit_rx: Some(exit_rx),
            journal,
            _root: root,
        };
        console.wait_screen(TEST_MODEL).await?;
        Ok(console)
    }

    async fn send(&self, bytes: Vec<u8>) -> Result<()> {
        self.session
            .writer_sender()
            .send(bytes)
            .await
            .context("write PTY input")
    }

    pub(super) async fn paste(&self, text: &str) -> Result<()> {
        self.send(format!("\x1b[200~{text}\x1b[201~").into_bytes())
            .await
    }

    pub(super) async fn type_text(&self, text: &str) -> Result<()> {
        self.send(text.as_bytes().to_vec()).await
    }

    pub(super) async fn enter(&self) -> Result<()> {
        self.send(b"\r".to_vec()).await
    }

    pub(super) async fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let size = TerminalSize { rows, cols };
        let (ack_tx, ack_rx) = oneshot::channel();
        self.resize_tx
            .send((size, ack_tx))
            .await
            .context("resize terminal emulator")?;
        ack_rx
            .await
            .context("terminal emulator stopped before resize")?;
        self.session.resize(size)
    }

    pub(super) async fn resize_and_wait(&mut self, rows: u16, cols: u16) -> Result<()> {
        self.resize(rows, cols).await?;
        // The top bar clears its last column on every draw. A width not used
        // earlier in this session proves the console rendered the final size,
        // rather than merely acknowledging an ioctl in this test process.
        let marker = format!("\x1b[1;{cols}H");
        self.wait_for(&format!("console redraw at {cols} columns"), |state| {
            state.output_tail.contains(&marker)
        })
        .await
    }

    pub(super) async fn wait_screen(&mut self, needle: &str) -> Result<()> {
        self.wait_for(&format!("screen containing {needle:?}"), |state| {
            state.screen.contains(needle)
        })
        .await
    }

    async fn wait_for(
        &mut self,
        description: &str,
        predicate: impl Fn(&TerminalSnapshot) -> bool,
    ) -> Result<()> {
        let outcome = tokio::time::timeout(
            WAIT,
            self.snapshots
                .wait_for(|state| predicate(state) || state.eof || state.error.is_some()),
        )
        .await;
        let visible = matches!(&outcome, Ok(Ok(state)) if predicate(state));
        drop(outcome);
        ensure!(
            visible,
            "waiting for {description} failed:\n{}",
            self.snapshots.borrow().diagnostic()
        );
        Ok(())
    }

    pub(super) async fn shutdown(&mut self) -> Result<()> {
        self.send(vec![4]).await?; // Ctrl+D, with an empty composer.
        let exit_rx = self.exit_rx.take().context("console already shut down")?;
        let exit = tokio::time::timeout(WAIT, exit_rx).await;
        ensure!(
            matches!(exit, Ok(Ok(0))),
            "console did not exit cleanly: {exit:?}\n{}",
            self.snapshots.borrow().diagnostic()
        );
        let state = tokio::time::timeout(WAIT, self.snapshots.wait_for(|state| state.eof))
            .await
            .context("console exited but PTY output did not close")??;
        ensure!(
            !state.cursor_hidden,
            "console left the terminal cursor hidden"
        );
        ensure!(
            !state.alternate_screen,
            "console left the alternate screen active"
        );
        ensure!(
            state.output_tail.contains("\x1b[?2004l"),
            "console did not disable bracketed paste on exit"
        );
        drop(state);
        if self.journal.try_wait()?.is_none() {
            self.journal.kill().await?;
        }
        self.journal.wait().await?;
        Ok(())
    }
}

impl Drop for PtyConsole {
    fn drop(&mut self) {
        self.session.request_terminate();
        self.capture.abort();
        // ProcessHandle and kill_on_drop reap/stop only the children we own.
    }
}

#[cfg(test)]
#[path = "harness/tests.rs"]
mod tests;
