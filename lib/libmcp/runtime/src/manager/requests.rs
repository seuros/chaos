use super::*;

impl McpConnectionManager {
    pub(super) async fn list_page<T: serde::de::DeserializeOwned>(
        &self,
        server: &str,
        method: &str,
        params: Option<PaginatedRequestParams>,
    ) -> Result<T> {
        let managed = self.client_by_name(server).await?;
        let params = params.unwrap_or(PaginatedRequestParams { cursor: None });
        managed
            .session
            .request(method, &params)
            .await
            .map_err(anyhow::Error::from)
            .with_context(|| format!("{method} failed for `{server}`"))
    }
}

pub(super) fn tool_arguments(
    arguments: Option<serde_json::Value>,
    operation: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    match arguments {
        None => Ok(None),
        Some(serde_json::Value::Object(map)) => Ok(Some(map)),
        Some(other) => Err(anyhow!(
            "{operation} arguments must be a JSON object, got {}",
            other.to_string().chars().take(80).collect::<String>()
        )),
    }
}

pub(super) async fn collect_server_results<T: Send + 'static>(
    mut calls: JoinSet<(String, Result<T>)>,
    operation: &str,
) -> HashMap<String, T> {
    let mut results = HashMap::new();
    while let Some(result) = calls.join_next().await {
        match result {
            Ok((server, Ok(value))) => {
                results.insert(server, value);
            }
            Ok((server, Err(error))) => {
                warn!(
                    server,
                    operation,
                    error = %format!("{error:#}"),
                    "MCP server operation failed"
                );
            }
            Err(error) => warn!(operation, %error, "MCP server operation task failed"),
        }
    }
    results
}

pub(super) async fn finish_notifications(mut calls: JoinSet<Result<()>>, notification: &str) {
    while let Some(result) = calls.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                warn!(notification, error = %format!("{error:#}"), "MCP notification failed");
            }
            Err(error) => warn!(notification, %error, "MCP notification task failed"),
        }
    }
}

#[cfg(test)]
mod tests;
