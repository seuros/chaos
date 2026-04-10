#![allow(clippy::expect_used)]
use chaos_kern::auth::CHAOS_API_KEY_ENV_VAR;
use std::path::Path;
use tempfile::TempDir;
use wiremock::MockServer;

pub struct TestChaosExecBuilder {
    home: TempDir,
    cwd: TempDir,
}

impl TestChaosExecBuilder {
    pub fn cmd(&self) -> assert_cmd::Command {
        let mut cmd = assert_cmd::Command::new(
            chaos_which::cargo_bin("chaos").expect("should find binary for chaos"),
        );
        cmd.current_dir(self.cwd.path())
            .arg("exec")
            .env("CHAOS_HOME", self.home.path())
            .env(CHAOS_API_KEY_ENV_VAR, "dummy");
        cmd
    }
    pub fn cmd_with_server(&self, server: &MockServer) -> assert_cmd::Command {
        let mut cmd = self.cmd();
        let base = format!("{}/v1", server.uri());
        cmd.arg("-c")
            .arg(format!("openai_base_url={}", toml_string_literal(&base)));
        cmd
    }

    pub fn cwd_path(&self) -> &Path {
        self.cwd.path()
    }
    pub fn home_path(&self) -> &Path {
        self.home.path()
    }
}

fn toml_string_literal(value: &str) -> String {
    serde_json::to_string(value).expect("serialize TOML string literal")
}

pub fn test_chaos_fork() -> TestChaosExecBuilder {
    TestChaosExecBuilder {
        home: TempDir::new().expect("create temp home"),
        cwd: TempDir::new().expect("create temp cwd"),
    }
}
