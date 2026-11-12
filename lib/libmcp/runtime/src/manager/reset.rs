use super::*;

/// Immutable connection inputs survive resets; sandbox state and roots are
/// copied from the current client rather than its original startup inputs.
pub(super) struct ClientSetup {
    pub(super) server_name: String,
    pub(super) config: McpServerConfig,
    pub(super) client_identity: McpClientIdentity,
    pub(super) store_mode: OAuthCredentialsStoreMode,
    pub(super) tx_event: Sender<Event>,
    pub(super) notification_tx: Option<Sender<McpServerNotification>>,
    pub(super) elicitation_requests: ElicitationRequestManager,
    pub(super) catalog: Arc<dyn McpCatalogSink>,
    pub(super) chaos_home: std::path::PathBuf,
}

impl AsyncManagedClient {
    fn replacement(&self) -> Result<Self> {
        let setup = self
            .setup
            .as_ref()
            .ok_or_else(|| anyhow!("MCP client has no connection setup"))?;
        Ok(Self::new(
            setup.server_name.clone(),
            setup.config.clone(),
            setup.client_identity.clone(),
            setup.store_mode,
            self.generation_token.clone(),
            setup.tx_event.clone(),
            setup.notification_tx.clone(),
            setup.elicitation_requests.clone(),
            Arc::clone(&setup.catalog),
            self.sandbox_state_snapshot(),
            setup.chaos_home.clone(),
        ))
    }

    pub(super) async fn shutdown(&self, server: &str) -> Result<()> {
        if let Some(catalog) = &self.catalog {
            catalog.retire(server);
        }
        self.cancel_token.cancel();
        tokio::time::timeout(MCP_SERVER_SHUTDOWN_TIMEOUT, async {
            match self.client().await {
                Ok(client) => match client.session.disconnect().await {
                    Ok(()) | Err(mcp_guest::GuestError::Disconnected) => Ok(()),
                    Err(error) => Err(anyhow!("failed to disconnect MCP server {server}: {error}")),
                },
                Err(StartupOutcomeError::Cancelled | StartupOutcomeError::Failed { .. }) => Ok(()),
            }
        })
        .await
        .with_context(|| format!("timed out shutting down MCP server {server}"))?
    }
}

impl McpConnectionManager {
    /// Reconnect one enabled server using this session's effective configuration.
    ///
    /// The registry must serialize this with calls to the named server and with
    /// generation replacement. Other clients and their transports are untouched.
    pub async fn reset_server(&self, server: &str) -> Result<()> {
        let previous = self
            .async_client(server)
            .ok_or_else(|| anyhow!("unknown or disabled MCP server `{server}`"))?;
        let replacement = previous.replacement()?;
        previous.shutdown(server).await?;
        self.elicitation_requests
            .requests
            .lock()
            .await
            .retain(|(name, _), _| name != server);
        self.clients
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(server.to_string(), replacement.clone());
        let setup = replacement
            .setup
            .as_ref()
            .ok_or_else(|| anyhow!("replacement MCP client has no connection setup"))?;
        emit_update(
            &setup.tx_event,
            McpStartupUpdateEvent {
                server: server.to_string(),
                status: McpStartupStatus::Starting,
            },
        )
        .await;
        let outcome = replacement.client().await;
        let status = match &outcome {
            Ok(client) => {
                let tools = qualify_tools(client.listed_tools())
                    .into_values()
                    .map(|tool| crate::catalog_conv::mcp_tool_info_to_catalog_tool(&tool))
                    .collect();
                if let Some(catalog) = &replacement.catalog {
                    catalog.replace_tools(server, tools);
                }
                if let Err(error) = replacement.notify_current_sandbox_state().await {
                    warn!(server, %error, "failed to notify sandbox state after MCP reset");
                }
                McpStartupStatus::Ready
            }
            Err(error) => {
                if let Some(catalog) = &replacement.catalog {
                    catalog.retire(server);
                }
                McpStartupStatus::Failed {
                    error: mcp_init_error_display(server, None, error),
                }
            }
        };
        emit_update(
            &setup.tx_event,
            McpStartupUpdateEvent {
                server: server.to_string(),
                status,
            },
        )
        .await;
        outcome
            .map(|_| ())
            .map_err(|error| anyhow!(startup_outcome_error_message(error)))
    }
}
