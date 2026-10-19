//! Exercises the real official ACP runtime without credentials or model calls.
//!
//! CHAOS_AGY_ACP_PATH=/path/to/agy_acp_server.par \
//! cargo test -p acp-guest --test antigravity_live -- --ignored --nocapture
#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use acp_guest::protocol::schema::v1::{ErrorCode, NewSessionRequest};
use acp_guest::{AcpConnection, ConnectionOptions, GuestError};
use tokio::process::{Child, Command};

/// Owns the process group even if an assertion unwinds. The live runtime may
/// launch localharness_external, which must not be left running after this test.
struct Agent {
    child: Child,
    group: Option<i32>,
}

impl Agent {
    fn spawn(path: &Path, home: &Path) -> io::Result<Self> {
        let mut command = Command::new(path);
        command
            .env_clear()
            .env("HOME", home)
            .env("GEMINI_HOME", home.join(".gemini"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("PATH", "/usr/bin:/bin")
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Do not capture or echo agent diagnostics that could later include
            // prompts, credentials or provider-private information.
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true);
        let child = command.spawn()?;
        let group = child.id().and_then(|id| i32::try_from(id).ok());
        Ok(Self { child, group })
    }

    fn kill_group(&mut self) -> io::Result<()> {
        if let Some(group) = self.group {
            // SAFETY: this is the child's dedicated group, established before
            // spawn returns. The group is killed before its leader is reaped.
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

    async fn stop(mut self) -> io::Result<()> {
        self.kill_group()?;
        tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .map_err(io::Error::other)??;
        Ok(())
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.kill_group();
    }
}

#[ignore = "requires the official Antigravity ACP runtime and its adjacent localharness_external"]
#[tokio::test]
async fn real_antigravity_negotiates_v1_and_requires_authentication() {
    let path = std::env::var_os("CHAOS_AGY_ACP_PATH")
        .expect("CHAOS_AGY_ACP_PATH must point to the official agy_acp_server.par");
    let home = tempfile::tempdir().expect("isolated profile");
    let mut agent = Agent::spawn(Path::new(&path), home.path()).expect("spawn official runtime");
    let stdin = agent.child.stdin.take().expect("agent stdin");
    let stdout = agent.child.stdout.take().expect("agent stdout");
    let (connection, mut updates) =
        AcpConnection::connect(stdout, stdin, ConnectionOptions::default())
            .await
            .expect("initialize official ACP runtime");
    let observer = tokio::spawn(async move { while updates.recv().await.is_some() {} });
    let initialization = connection.initialized();
    let info = initialization
        .agent_info
        .as_ref()
        .expect("official agent identification");
    assert_eq!(info.name, "antigravity-acp");
    assert!(initialization.agent_capabilities.load_session);
    assert!(
        initialization
            .auth_methods
            .iter()
            .any(|method| method.id().to_string() == "oauth-personal")
    );
    eprintln!("ACP initialized: {} {}", info.name, info.version);

    // No automatic authentication, browser login, imported OAuth state or API-key
    // fallback. Failure here must remain an explicit authentication boundary.
    let result = connection
        .client()
        .request(NewSessionRequest::new(home.path().to_path_buf()))
        .await;
    assert!(
        matches!(
            &result,
            Err(GuestError::Protocol(error)) if error.code == ErrorCode::AuthRequired
        ),
        "empty dedicated profile must require authentication: {result:?}"
    );
    connection.disconnect().await.expect("close protocol");
    observer.await.expect("join observer");
    agent.stop().await.expect("kill and reap runtime group");
}
