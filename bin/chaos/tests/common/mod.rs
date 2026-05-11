#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use anyhow::Result;
use chaos_kern::config::load_global_mcp_servers;
use chaos_kern::config::replace_global_mcp_servers;
use chaos_kern::config::types::McpServerConfig;
use chaos_kern::config::types::McpServerTransportConfig;
use chaos_kern::config::upsert_global_mcp_server;
use predicates::str::contains;
use tempfile::TempDir;

pub fn chaos_command(chaos_home: &Path) -> Result<assert_cmd::Command> {
    let mut cmd = assert_cmd::Command::new(chaos_which::cargo_bin("chaos")?);
    cmd.env("CHAOS_HOME", chaos_home);
    Ok(cmd)
}

pub struct McpCliHarness {
    pub chaos_home: TempDir,
}

impl McpCliHarness {
    pub fn new() -> Result<Self> {
        Ok(Self {
            chaos_home: TempDir::new()?,
        })
    }

    pub fn command(&self) -> Result<assert_cmd::Command> {
        chaos_command(self.chaos_home.path())
    }

    pub fn assert_success(&self, args: &[&str]) -> Result<()> {
        self.command()?.args(args).assert().success();
        Ok(())
    }

    pub fn assert_success_stdout(&self, args: &[&str], expected_stdout: &str) -> Result<()> {
        self.command()?
            .args(args)
            .assert()
            .success()
            .stdout(contains(expected_stdout));
        Ok(())
    }

    pub fn assert_failure_stderr(&self, args: &[&str], expected_stderr: &str) -> Result<()> {
        self.command()?
            .args(args)
            .assert()
            .failure()
            .stderr(contains(expected_stderr));
        Ok(())
    }

    pub fn stdout(&self, args: &[&str]) -> Result<String> {
        let output = self.command()?.args(args).output()?;
        assert!(output.status.success());
        Ok(String::from_utf8(output.stdout)?)
    }

    pub async fn servers(&self) -> Result<BTreeMap<String, McpServerConfig>> {
        Ok(load_global_mcp_servers(self.chaos_home.path()).await?)
    }

    pub async fn server(&self, name: &str) -> Result<McpServerConfig> {
        let mut servers = self.servers().await?;
        servers
            .remove(name)
            .with_context(|| format!("server should exist: {name}"))
    }

    pub async fn assert_no_servers(&self) -> Result<()> {
        assert!(self.servers().await?.is_empty());
        Ok(())
    }

    pub async fn insert_server(&self, name: &str, server: McpServerConfig) -> Result<()> {
        upsert_global_mcp_server(self.chaos_home.path(), name, &server).await?;
        Ok(())
    }

    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        self.update_servers(|servers| {
            let server = servers
                .get_mut(name)
                .with_context(|| format!("server should exist after add: {name}"))?;
            server.enabled = enabled;
            Ok(())
        })
        .await
    }

    pub async fn set_stdio_env_vars(&self, name: &str, env_vars: &[&str]) -> Result<()> {
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

    pub async fn update_servers(
        &self,
        update: impl FnOnce(&mut BTreeMap<String, McpServerConfig>) -> Result<()>,
    ) -> Result<()> {
        let mut servers = self.servers().await?;
        update(&mut servers)?;
        replace_global_mcp_servers(self.chaos_home.path(), &servers)?;
        Ok(())
    }
}
