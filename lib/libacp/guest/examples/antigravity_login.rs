//! Official, explicitly requested OAuth login in an existing dedicated profile.
//! Does not read/copy credentials, send prompts, or configure model tools.
//!
//! cargo run -p acp-guest --example antigravity_login -- SERVER PRIVATE_HOME
#![cfg(unix)]

use std::time::Duration;

use acp_guest::protocol::schema::v1::AuthenticateRequest;
use acp_guest::{AcpConnection, ConnectionOptions};
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
                request_timeout: Duration::from_secs(600),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| "could not initialize the official ACP runtime")?;
        let method = connection
            .initialized()
            .auth_methods
            .iter()
            .find(|method| method.id().to_string() == "oauth-personal")
            .ok_or("runtime does not offer personal Google OAuth")?
            .id()
            .clone();
        let observer = tokio::spawn(async move { while updates.recv().await.is_some() {} });
        eprintln!("Opening official Google OAuth. Complete sign-in in your browser.");
        let authenticated = connection
            .client()
            .request(AuthenticateRequest::new(method))
            .await;
        let closed = connection.disconnect().await;
        let observed = observer.await;
        let authenticated = authenticated
            .map_err(|_| "official OAuth failed or timed out; no credentials were copied")?;
        if let Some(meta) = &authenticated.meta {
            eprintln!(
                "Authentication response metadata keys (values withheld): {:?}",
                meta.keys().collect::<Vec<_>>()
            );
        }
        closed.map_err(|_| "OAuth returned, but the ACP connection did not close cleanly")?;
        observed.map_err(|_| "ACP observer did not stop cleanly")?;
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
    result?;
    eprintln!(
        "Official authenticate request returned successfully. \
         Verify session creation separately; no model request was made."
    );
    Ok(())
}
