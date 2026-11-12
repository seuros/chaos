use super::*;

impl McpRegistryActor {
    /// Must run through the session's refresh actor, which serializes resets
    /// against configuration reloads and full generation replacement.
    pub(crate) async fn reset_server(&self, server: &str) -> anyhow::Result<()> {
        let configs = self.configs_snapshot();
        let config = configs
            .get(server)
            .ok_or_else(|| anyhow::anyhow!("unknown MCP server `{server}`"))?;
        if !config.enabled {
            anyhow::bail!("MCP server `{server}` is disabled; enable it before resetting");
        }
        let configs = HashMap::from([(server.to_string(), config.clone())]);
        let subscriptions = Arc::clone(&self.resource_subscriptions);
        let revision = Arc::clone(&self.revision);
        self.execute(server, move |manager, server| async move {
            let result = manager.reset_server(&server).await;
            revision
                .fetch_add(1, Ordering::SeqCst)
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("MCP registry revision overflow"))?;
            result?;
            restore_resource_subscriptions(&manager, &configs, &subscriptions).await;
            Ok(())
        })
        .await
    }
}
