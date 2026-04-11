use std::collections::BTreeMap;

use anyhow::Context;
use anyhow::Result;
use chaos_kern::config::edit::ConfigEditsBuilder;
use chaos_kern::config::load_global_mcp_servers;
use chaos_kern::config::types::McpServerConfig;
use chaos_kern::config::types::McpServerTransportConfig;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use pretty_assertions::assert_eq;
use serde_json::Value as JsonValue;
use serde_json::json;
use tempfile::TempDir;

mod common;

use common::chaos_command;

struct McpCliHarness {
    chaos_home: TempDir,
}

impl McpCliHarness {
    fn new() -> Result<Self> {
        Ok(Self {
            chaos_home: TempDir::new()?,
        })
    }

    fn assert_success(&self, args: &[&str]) -> Result<()> {
        self.command()?.args(args).assert().success();
        Ok(())
    }

    fn stdout(&self, args: &[&str]) -> Result<String> {
        let output = self.command()?.args(args).output()?;
        assert!(output.status.success());
        Ok(String::from_utf8(output.stdout)?)
    }

    async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.update_servers(|servers| {
            let server = servers
                .get_mut(name)
                .with_context(|| format!("server should exist after add: {name}"))?;
            server.enabled = enabled;
            Ok(())
        })
        .await
    }

    async fn set_stdio_env_vars(&self, name: &str, env_vars: &[&str]) -> Result<()> {
        self.update_servers(|servers| {
            let server = servers
                .get_mut(name)
                .with_context(|| format!("server should exist after add: {name}"))?;
            match &mut server.transport {
                McpServerTransportConfig::Stdio {
                    env_vars: stored_env_vars,
                    ..
                } => {
                    *stored_env_vars = env_vars
                        .iter()
                        .map(|env_var| (*env_var).to_string())
                        .collect();
                }
                other => panic!("unexpected transport: {other:?}"),
            }
            Ok(())
        })
        .await
    }

    fn command(&self) -> Result<assert_cmd::Command> {
        chaos_command(self.chaos_home.path())
    }

    async fn servers(&self) -> Result<BTreeMap<String, McpServerConfig>> {
        Ok(load_global_mcp_servers(self.chaos_home.path()).await?)
    }

    async fn update_servers(
        &self,
        update: impl FnOnce(&mut BTreeMap<String, McpServerConfig>) -> Result<()>,
    ) -> Result<()> {
        let mut servers = self.servers().await?;
        update(&mut servers)?;
        ConfigEditsBuilder::new(self.chaos_home.path())
            .replace_mcp_servers(&servers)
            .apply_blocking()?;
        Ok(())
    }
}

#[test]
fn list_shows_empty_state() -> Result<()> {
    let harness = McpCliHarness::new()?;

    let stdout = harness.stdout(&["mcp", "list"])?;
    assert!(stdout.contains("No MCP servers configured yet."));

    Ok(())
}

#[tokio::test]
async fn list_and_get_render_expected_output() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_success(&[
        "mcp",
        "add",
        "docs",
        "--env",
        "TOKEN=secret",
        "--",
        "docs-server",
        "--port",
        "4000",
    ])?;
    harness
        .set_stdio_env_vars("docs", &["APP_TOKEN", "WORKSPACE_ID"])
        .await?;

    let stdout = harness.stdout(&["mcp", "list"])?;
    assert!(stdout.contains("Name"));
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("docs-server"));
    assert!(stdout.contains("TOKEN=*****"));
    assert!(stdout.contains("APP_TOKEN=*****"));
    assert!(stdout.contains("WORKSPACE_ID=*****"));
    assert!(stdout.contains("Status"));
    assert!(stdout.contains("Auth"));
    assert!(stdout.contains("enabled"));
    // Auth column shows "-" for stdio servers that don't support OAuth.
    assert!(
        stdout.lines().skip(1).all(|line| line.contains("-")),
        "expected '-' in auth column for stdio server"
    );

    let stdout = harness.stdout(&["mcp", "list", "--json"])?;
    let parsed: JsonValue = serde_json::from_str(&stdout)?;
    assert_eq!(
        parsed,
        json!([
          {
            "name": "docs",
            "enabled": true,
            "disabled_reason": null,
            "transport": {
              "type": "stdio",
              "command": "docs-server",
              "args": [
                "--port",
                "4000"
              ],
              "env": {
                "TOKEN": "secret"
              },
              "env_vars": [
                "APP_TOKEN",
                "WORKSPACE_ID"
              ],
              "cwd": null
            },
            "startup_timeout_sec": null,
            "tool_timeout_sec": null,
            "auth_status": "unsupported"
          }
        ]
        )
    );

    let stdout = harness.stdout(&["mcp", "get", "docs"])?;
    assert!(stdout.contains("docs"));
    assert!(stdout.contains("transport: stdio"));
    assert!(stdout.contains("command: docs-server"));
    assert!(stdout.contains("args: --port 4000"));
    assert!(stdout.contains("env: TOKEN=*****"));
    assert!(stdout.contains("APP_TOKEN=*****"));
    assert!(stdout.contains("WORKSPACE_ID=*****"));
    assert!(stdout.contains("enabled: true"));
    assert!(stdout.contains("remove: chaos mcp remove docs"));

    harness
        .command()?
        .args(["mcp", "get", "docs", "--json"])
        .assert()
        .success()
        .stdout(contains("\"name\": \"docs\"").and(contains("\"enabled\": true")));

    Ok(())
}

#[tokio::test]
async fn get_disabled_server_shows_single_line() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_success(&["mcp", "add", "docs", "--", "docs-server"])?;
    harness.set_enabled("docs", false).await?;

    let stdout = harness.stdout(&["mcp", "get", "docs"])?;
    assert_eq!(stdout.trim_end(), "docs (disabled)");

    Ok(())
}
