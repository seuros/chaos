//! Transport-neutral server construction and bounded ownership cleanup.

use std::io::Result as IoResult;
use std::sync::Arc;
use std::time::Duration;

use chaos_argv::Arg0DispatchPaths;
use chaos_ipc::product::{CHAOS_VERSION, OS_NAME};
use chaos_kern::AuthManager;
use chaos_kern::config::Config;
use mcp_host::prelude::*;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::chaos_tool::ChaosMcpServer;
use crate::session_state::{CLEANUP_TIMEOUT, SessionStates, cleanup_sessions};
use crate::{CHAOS_WEBSITE_URL, MCP_SERVER_DESCRIPTION};

/// Retain this handle for the entire transport lifetime, then stop the
/// transport and await `shutdown`. Dropping it also closes all owners and
/// signals bounded cleanup; the worker never owns the SDK server.
pub struct McpServerHandle {
    pub server: Arc<Server>,
    states: Arc<SessionStates>,
    stop: Option<oneshot::Sender<()>>,
    cleanup: Option<JoinHandle<IoResult<()>>>,
}

impl McpServerHandle {
    pub async fn shutdown(&mut self) -> IoResult<()> {
        self.close_sessions();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(mut cleanup) = self.cleanup.take() {
            match tokio::time::timeout(CLEANUP_TIMEOUT + Duration::from_secs(1), &mut cleanup).await
            {
                Ok(result) => result.map_err(std::io::Error::other)??,
                Err(_) => {
                    cleanup.abort();
                    let _ = cleanup.await;
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "MCP ownership cleanup timed out",
                    ));
                }
            }
        }
        Ok(())
    }

    fn close_sessions(&self) {
        self.states.close();
        // Also revoke SDK tasks, cancellation scopes and pending client routes.
        // This is whole-server shutdown, never a response to SSE disconnection.
        for session in self.server.active_sessions() {
            self.server.remove_session(session.id());
        }
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        self.close_sessions();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // The worker owns no strong server reference and exits on this signal.
        // Do not abort it here: it must send the owned process shutdowns.
    }
}

pub async fn build_server(
    config: Arc<Config>,
    arg0_paths: Arg0DispatchPaths,
) -> IoResult<McpServerHandle> {
    // HTTP embeds the same process factory without stdio startup. It needs
    // the same recursive-sidecar defense, but must not mutate caller config.
    let mut isolated_config = config.as_ref().clone();
    crate::strip_chaos_self_reference_from_mcp_servers(&mut isolated_config);
    let config = Arc::new(isolated_config);
    chaos_kern::runtime_db::mount_vfs_for_startup(&config)
        .await
        .map_err(std::io::Error::other)?;
    let runtime_db = chaos_kern::runtime_db::get_runtime_db(&config);
    let auth_manager = AuthManager::shared(
        config.chaos_home.clone(),
        false,
        config.cli_auth_credentials_store_mode,
    );
    let server = Arc::new(
        Server::builder("chaos-mcp-server", CHAOS_VERSION)
            .with_title(OS_NAME)
            .with_description(MCP_SERVER_DESCRIPTION)
            .with_website_url(CHAOS_WEBSITE_URL.parse().map_err(std::io::Error::other)?)
            .with_tools(true)
            .with_resources(true, false)
            .with_resource_templates()
            .with_tasks(true, true)
            .with_mcp_apps()
            .with_runtime_config(ServerRuntimeConfig::default())
            .with_instructions("Chaos — provider-agnostic coding agent")
            .build(),
    );
    let states = Arc::new(SessionStates::default());
    let chaos_server = Arc::new(ChaosMcpServer {
        config,
        auth_manager,
        states: states.clone(),
        mcp_server: Arc::downgrade(&server),
        arg0_paths,
        runtime_db,
    });
    crate::chaos_tool::tool_router()
        .register_all(server.tool_registry(), chaos_server.clone())
        .map_err(std::io::Error::other)?;
    crate::models_tool::tool_router()
        .register_all(server.tool_registry(), chaos_server.clone())
        .map_err(std::io::Error::other)?;
    crate::workflows::register(&server, chaos_server.clone()).map_err(std::io::Error::other)?;
    crate::builtin_resources::resource_router()
        .register_all(server.resource_manager(), chaos_server.clone())
        .map_err(std::io::Error::other)?;
    crate::builtin_resources::resource_template_router()
        .register_all(server.resource_manager(), chaos_server)
        .map_err(std::io::Error::other)?;

    let (stop, mut stopped) = oneshot::channel();
    let weak_server = Arc::downgrade(&server);
    let owned_states = states.clone();
    let cleanup = tokio::spawn(async move {
        let mut poll = tokio::time::interval(Duration::from_millis(250));
        let mut pending = tokio::task::JoinSet::new();
        let mut failure = None;
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                _ = poll.tick() => {
                    let Some(server) = weak_server.upgrade() else { break };
                    let expired = owned_states.expire(&server);
                    drop(server);
                    cleanup_sessions(&mut pending, expired);
                }
                result = pending.join_next(), if !pending.is_empty() => {
                    record_cleanup(result, &mut failure);
                }
            }
        }
        cleanup_sessions(&mut pending, owned_states.drain());
        while let Some(result) = pending.join_next().await {
            record_cleanup(Some(result), &mut failure);
        }
        failure.map_or(Ok(()), Err)
    });
    Ok(McpServerHandle {
        server,
        states,
        stop: Some(stop),
        cleanup: Some(cleanup),
    })
}

fn record_cleanup(
    result: Option<Result<IoResult<()>, tokio::task::JoinError>>,
    failure: &mut Option<std::io::Error>,
) {
    let error = match result {
        Some(Ok(Err(error))) => error,
        Some(Err(error)) => std::io::Error::other(error),
        _ => return,
    };
    tracing::warn!(%error, "MCP owned-session cleanup failed");
    if failure.is_none() {
        *failure = Some(error);
    }
}

#[cfg(test)]
mod tests;
