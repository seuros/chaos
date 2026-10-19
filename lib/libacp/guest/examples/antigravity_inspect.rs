//! Discover authenticated ACP session controls without sending model prompts.
//!
//! cargo run -p acp-guest --example antigravity_inspect -- SERVER PRIVATE_HOME
#![cfg(unix)]

use std::time::Duration;

use acp_guest::protocol::schema::v1::NewSessionRequest;
use acp_guest::{AcpConnection, ConnectionOptions, GuestError};

mod support;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (server, home) = support::parameters()?;
    let mut agent = support::Agent::spawn(&server, &home)?;
    let result = async {
        let stdin = agent.child.stdin.take().ok_or("missing agent stdin")?;
        let stdout = agent.child.stdout.take().ok_or("missing agent stdout")?;
        let (connection, mut updates) = AcpConnection::connect(
            stdout,
            stdin,
            ConnectionOptions {
                request_timeout: Duration::from_secs(120),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| "could not initialize the official ACP runtime")?;
        let observer = tokio::spawn(async move { while updates.recv().await.is_some() {} });
        if let Some(info) = &connection.initialized().agent_info {
            eprintln!("ACP initialized: {} {}", info.name, info.version);
        }
        // A temporary empty workspace avoids loading resident/project prompts.
        let workspace = tempfile::tempdir()?;
        let session = connection
            .client()
            .request(NewSessionRequest::new(workspace.path().to_path_buf()))
            .await;
        let closed = connection.disconnect().await;
        let observed = observer.await;
        let session = session.map_err(|error| match error {
            GuestError::Protocol(error) => format!("session/new failed (ACP code {})", error.code),
            GuestError::Timeout => "session/new timed out; no prompt was submitted".to_string(),
            _ => "session/new disconnected; no prompt was submitted".to_string(),
        })?;
        closed.map_err(|_| "ACP connection did not close cleanly")?;
        observed.map_err(|_| "ACP observer did not stop cleanly")?;
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "modes": session.modes,
                "configOptions": session.config_options,
            }))?
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    if result.is_err() {
        eprintln!(
            "Runtime diagnostic categories (not terminal proof): {:?}",
            agent.diagnostic_categories()
        );
    }
    agent.stop().await?;
    result
}
