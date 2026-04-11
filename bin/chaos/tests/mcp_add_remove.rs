use std::collections::BTreeMap;
use std::collections::HashMap;

use anyhow::Context;
use anyhow::Result;
use chaos_kern::config::load_global_mcp_servers;
use chaos_kern::config::types::McpServerConfig;
use chaos_kern::config::types::McpServerTransportConfig;
use predicates::str::contains;
use pretty_assertions::assert_eq;
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

    fn assert_success_stdout(&self, args: &[&str], expected_stdout: &str) -> Result<()> {
        self.command()?
            .args(args)
            .assert()
            .success()
            .stdout(contains(expected_stdout));
        Ok(())
    }

    fn assert_failure_stderr(&self, args: &[&str], expected_stderr: &str) -> Result<()> {
        self.command()?
            .args(args)
            .assert()
            .failure()
            .stderr(contains(expected_stderr));
        Ok(())
    }

    async fn server(&self, name: &str) -> Result<McpServerConfig> {
        let mut servers = self.servers().await?;
        servers
            .remove(name)
            .with_context(|| format!("server should exist: {name}"))
    }

    async fn assert_no_servers(&self) -> Result<()> {
        assert!(self.servers().await?.is_empty());
        Ok(())
    }

    fn command(&self) -> Result<assert_cmd::Command> {
        chaos_command(self.chaos_home.path())
    }

    async fn servers(&self) -> Result<BTreeMap<String, McpServerConfig>> {
        Ok(load_global_mcp_servers(self.chaos_home.path()).await?)
    }
}

fn assert_stdio_transport<'a>(
    server: &'a McpServerConfig,
    expected_command: &str,
    expected_args: &[&str],
    expected_env_vars: &[&str],
) -> Option<&'a HashMap<String, String>> {
    match &server.transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            env_vars,
            cwd,
        } => {
            assert_eq!(command, expected_command);
            assert_eq!(
                args,
                &expected_args
                    .iter()
                    .map(|arg| (*arg).to_string())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                env_vars,
                &expected_env_vars
                    .iter()
                    .map(|env_var| (*env_var).to_string())
                    .collect::<Vec<_>>()
            );
            assert!(cwd.is_none());
            assert!(server.enabled);
            env.as_ref()
        }
        other => panic!("unexpected transport: {other:?}"),
    }
}

fn assert_streamable_http_transport(
    server: &McpServerConfig,
    expected_url: &str,
    expected_bearer_token_env_var: Option<&str>,
) {
    match &server.transport {
        McpServerTransportConfig::StreamableHttp {
            url,
            bearer_token_env_var,
            http_headers,
            env_http_headers,
        } => {
            assert_eq!(url, expected_url);
            assert_eq!(
                bearer_token_env_var.as_deref(),
                expected_bearer_token_env_var
            );
            assert!(http_headers.is_none());
            assert!(env_http_headers.is_none());
        }
        other => panic!("unexpected transport: {other:?}"),
    }
    assert!(server.enabled);
}

#[tokio::test]
async fn add_and_remove_server_updates_global_config() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_success_stdout(
        &["mcp", "add", "docs", "--", "echo", "hello"],
        "Added global MCP server 'docs'.",
    )?;

    let docs = harness.server("docs").await?;
    assert_eq!(harness.servers().await?.len(), 1);
    assert!(
        assert_stdio_transport(&docs, "echo", &["hello"], &[]).is_none(),
        "stdio env should be empty"
    );

    harness.assert_success_stdout(
        &["mcp", "remove", "docs"],
        "Removed global MCP server 'docs'.",
    )?;
    harness.assert_no_servers().await?;

    harness.assert_success_stdout(
        &["mcp", "remove", "docs"],
        "No MCP server named 'docs' found.",
    )?;
    harness.assert_no_servers().await?;

    Ok(())
}

#[tokio::test]
async fn add_with_env_preserves_key_order_and_values() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_success(&[
        "mcp",
        "add",
        "envy",
        "--env",
        "FOO=bar",
        "--env",
        "ALPHA=beta",
        "--",
        "python",
        "server.py",
    ])?;

    let envy = harness.server("envy").await?;
    let env =
        assert_stdio_transport(&envy, "python", &["server.py"], &[]).context("env should exist")?;

    assert_eq!(env.len(), 2);
    assert_eq!(env.get("FOO"), Some(&"bar".to_string()));
    assert_eq!(env.get("ALPHA"), Some(&"beta".to_string()));

    Ok(())
}

#[tokio::test]
async fn add_streamable_http_without_manual_token() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_success(&["mcp", "add", "github", "--url", "https://example.com/mcp"])?;

    let github = harness.server("github").await?;
    assert_streamable_http_transport(&github, "https://example.com/mcp", None);

    assert!(!harness.chaos_home.path().join(".credentials.json").exists());
    assert!(!harness.chaos_home.path().join(".env").exists());

    Ok(())
}

#[tokio::test]
async fn add_streamable_http_with_custom_env_var() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_success(&[
        "mcp",
        "add",
        "issues",
        "--url",
        "https://example.com/issues",
        "--bearer-token-env-var",
        "GITHUB_TOKEN",
    ])?;

    let issues = harness.server("issues").await?;
    assert_streamable_http_transport(&issues, "https://example.com/issues", Some("GITHUB_TOKEN"));

    Ok(())
}

#[tokio::test]
async fn add_streamable_http_rejects_removed_flag() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_failure_stderr(
        &[
            "mcp",
            "add",
            "github",
            "--url",
            "https://example.com/mcp",
            "--with-bearer-token",
        ],
        "--with-bearer-token",
    )?;
    harness.assert_no_servers().await?;

    Ok(())
}

#[tokio::test]
async fn add_cant_add_command_and_url() -> Result<()> {
    let harness = McpCliHarness::new()?;

    harness.assert_failure_stderr(
        &[
            "mcp",
            "add",
            "github",
            "--url",
            "https://example.com/mcp",
            "--command",
            "--",
            "echo",
            "hello",
        ],
        "unexpected argument '--command' found",
    )?;
    harness.assert_no_servers().await?;

    Ok(())
}
