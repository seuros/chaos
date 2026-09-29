//! Real CLI provisioning and lifecycle dispatch, with a scripted local provider.
use anyhow::{Context, Result, ensure};
use core_test_support::responses::{
    self, ev_assistant_message, ev_completed, ev_function_call, sse,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::Command;

struct Fixture {
    root: tempfile::TempDir,
    binary: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self> {
        Ok(Self {
            root: tempfile::Builder::new()
                .prefix("hooks-")
                .tempdir_in("/tmp")?,
            binary: chaos_which::cargo_bin("chaos")?,
        })
    }
    fn path(&self) -> &Path {
        self.root.path()
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.binary);
        // Never inherit operator storage, agent identity, credentials, or provider routing.
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.path())
            .env("CHAOS_HOME", self.path())
            .current_dir(self.path())
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }
    async fn run(&self, args: &[&str]) -> Result<Output> {
        checked(self.command().args(args)).await
    }
    async fn hooks(&self) -> Result<Vec<Value>> {
        Ok(serde_json::from_slice(
            &self.run(&["hooks", "list"]).await?.stdout,
        )?)
    }
}

async fn checked(command: &mut Command) -> Result<Output> {
    let output = tokio::time::timeout(Duration::from_secs(30), command.output())
        .await
        .context("CLI timed out")??;
    ensure!(
        output.status.success(),
        "CLI failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

#[tokio::test]
async fn noninteractive_provisioning_is_explicit_and_agent_shells_cannot_approve() -> Result<()> {
    let f = Fixture::new()?;
    let add = [
        "hooks",
        "add",
        "managed",
        "--event",
        "before-turn",
        "--command",
        "printf managed",
        "--enabled",
    ];
    let denied = f.command().args(add).output().await?;
    ensure!(!denied.status.success());
    ensure!(f.hooks().await?.is_empty());
    let denied = f
        .command()
        .env("CHAOS_THREAD_ID", "test-agent")
        .args([
            "hooks",
            "--yes",
            "add",
            "managed",
            "--event",
            "before-turn",
            "--command",
            "printf managed",
            "--enabled",
        ])
        .output()
        .await?;
    ensure!(!denied.status.success());
    ensure!(
        String::from_utf8_lossy(&denied.stderr).contains("agent shell commands must use hooks_*")
    );
    ensure!(f.hooks().await?.is_empty());
    f.run(&[
        "hooks",
        "--yes",
        "add",
        "managed",
        "--event",
        "before-turn",
        "--command",
        "printf managed",
        "--enabled",
    ])
    .await?;
    ensure!(f.hooks().await?[0]["approved"] == true);
    f.run(&[
        "hooks",
        "--yes",
        "update",
        "managed",
        r#"{"event":"stop","command":"printf changed"}"#,
    ])
    .await?;
    ensure!(f.hooks().await?[0]["definition"]["command"] == "printf changed");
    f.run(&["hooks", "--yes", "disable", "managed"]).await?;
    ensure!(f.hooks().await?[0]["enabled"] == false);
    f.run(&["hooks", "--yes", "remove", "managed"]).await?;
    ensure!(f.hooks().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn imported_and_resident_hooks_execute_after_restart_without_human_endpoint() -> Result<()> {
    let f = Fixture::new()?;
    let mut hooks = serde_json::Map::new();
    for event in ["SessionStart", "BeforeTurn", "Stop"] {
        hooks.insert(
            event.into(),
            json!([{"hooks":[{"type":"command", "command":
            format!("cat >/dev/null; printf '{event}\\n' >> events"), "timeout":5}]}]),
        );
    }
    let legacy = f.path().join("hooks.json");
    std::fs::write(&legacy, serde_json::to_vec(&json!({"hooks":hooks}))?)?;
    f.run(&[
        "hooks",
        "--yes",
        "import",
        legacy.to_str().context("UTF-8 path")?,
        "--prefix",
        "legacy",
    ])
    .await?;
    ensure!(
        f.hooks()
            .await?
            .iter()
            .all(|h| h["enabled"] == false && h["approved"] == false)
    );
    for id in ["legacy-1", "legacy-2", "legacy-3"] {
        f.run(&["hooks", "--yes", "enable", id]).await?;
    }
    // A second process reads the same durable grants; legacy source is left intact.
    ensure!(legacy.exists());
    ensure!(f.hooks().await?.iter().all(|h| h["approved"] == true));
    f.run(&["config", "set", "hook_approval_policy", "\"automatic\""])
        .await?;
    let catalog = f.path().join("models.json");
    std::fs::write(
        &catalog,
        serde_json::to_vec(&json!({"models":[
            chaos_kern::test_support::test_model_info("gpt-5.1")
        ]}))?,
    )?;
    let server = responses::start_mock_server().await;
    let resident = json!({"id":"resident", "enabled":true, "definition":{
        "event":"stop", "command":"cat >/dev/null; printf 'resident\\n' >> events"
    }});
    responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_function_call("enable", "enable_tools", r#"{"groups":["session"]}"#),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_function_call("create", "hooks_create", &resident.to_string()),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m1", "ready"),
                ev_completed("r3"),
            ]),
            sse(vec![
                ev_assistant_message("m2", "restarted"),
                ev_completed("r4"),
            ]),
        ],
    )
    .await;
    let socket = f.path().join("run/journald.sock");
    let mut daemon = Command::new(chaos_which::cargo_bin("chaos_journald")?)
        .args(["--socket"])
        .arg(&socket)
        .args(["--db"])
        .arg(f.path().join("journal.sqlite"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let result = async {
        let client = chaos_journald::JournalRpcClient::new(socket);
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            loop {
                tick.tick().await;
                if client.hello("hook-regression").await.is_ok() {
                    return Ok::<_, anyhow::Error>(());
                }
                ensure!(
                    daemon.try_wait()?.is_none(),
                    "journald exited before readiness"
                );
            }
        })
        .await
        .context("journald readiness timed out")??;
        for _ in 0..2 {
            checked(f.command().args([
                "exec",
                "--json",
                "--headless",
                "--skip-git-repo-check",
                "-c",
                "machine_warnings.enabled=false",
                "-c",
                "disable_user_scripts=true",
                "-c",
                "model_provider=fixture",
                "-c",
                "model_providers.fixture.name=Fixture",
                "-c",
                "model_providers.fixture.wire_api=responses",
                "-c",
                &format!("model_providers.fixture.base_url={}/v1", server.uri()),
                "-c",
                &format!("model_catalog_json={}", catalog.display()),
                "-m",
                "gpt-5.1",
                "test hooks",
            ]))
            .await?;
        }
        let mut events: Vec<_> = std::fs::read_to_string(f.path().join("events"))?
            .lines()
            .map(str::to_owned)
            .collect();
        events.sort();
        ensure!(
            events
                == [
                    "BeforeTurn",
                    "BeforeTurn",
                    "SessionStart",
                    "SessionStart",
                    "Stop",
                    "Stop",
                    "resident",
                    "resident"
                ],
            "unexpected dispatch: {events:?}"
        );
        ensure!(f.hooks().await?.len() == 4);
        f.run(&["approvals", "revoke", "all"]).await?;
        ensure!(f.hooks().await?.iter().all(|h| h["approved"] == false));
        Ok(())
    }
    .await;
    let _ = daemon.kill().await;
    let _ = daemon.wait().await;
    result
}
