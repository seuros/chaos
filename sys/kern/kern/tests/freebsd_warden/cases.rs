use anyhow::{Context, Result, bail, ensure};
use chaos_ipc::permissions::{SocketPolicy, VfsPolicy};
use chaos_ipc::protocol::{ApprovalPolicy, EventMsg, Op, SandboxPolicy};
use chaos_ipc::user_input::UserInput;
use chaos_kern::error::{ChaosErr, SandboxErr};
use chaos_kern::exec::{
    ExecExpiration, ExecParams, ExecToolCallOutput, StdoutStream, process_exec_tool_call,
};
use chaos_kern::sandboxing::SandboxPermissions;
use core_test_support::responses::{
    ev_assistant_message, ev_completed, ev_function_call, ev_response_created, mount_sse_sequence,
    sse,
};
use core_test_support::test_chaos::{TestChaosHarness, test_chaos};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn params(command: Vec<String>, cwd: &Path, expiration: ExecExpiration) -> ExecParams {
    ExecParams {
        command,
        cwd: cwd.to_path_buf(),
        expiration,
        env: HashMap::from([("PATH".into(), "/bin:/usr/bin".into())]),
        network: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        justification: None,
        arg0: None,
    }
}

async fn oneshot(helper: &Path, cwd: &Path, command: Vec<String>) -> Result<ExecToolCallOutput> {
    Ok(process_exec_tool_call(
        params(command, cwd, 10_000.into()),
        &VfsPolicy::default(),
        SocketPolicy::Restricted,
        cwd,
        helper,
        None,
    )
    .await?)
}

fn shell(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

async fn cancellation(helper: &Path, cwd: &Path) -> Result<()> {
    let cancel = CancellationToken::new();
    let (tx_event, rx_event) = async_channel::bounded(64);
    let policy = VfsPolicy::default();
    let request = process_exec_tool_call(
        params(
            shell("sleep 30 & printf 'cancel-ready\\n'; wait"),
            cwd,
            ExecExpiration::Cancellation(cancel.clone()),
        ),
        &policy,
        SocketPolicy::Restricted,
        cwd,
        helper,
        Some(StdoutStream {
            sub_id: "warden-cancel".into(),
            call_id: "warden-cancel".into(),
            tx_event,
        }),
    );
    let ready = async {
        let mut output = Vec::new();
        while let Ok(event) = rx_event.recv().await {
            if let EventMsg::ExecCommandOutputDelta(delta) = event.msg {
                output.extend(delta.chunk);
                if output
                    .windows(b"cancel-ready\n".len())
                    .any(|w| w == b"cancel-ready\n")
                {
                    cancel.cancel();
                    return Ok(());
                }
            }
        }
        bail!("oneshot exited before cancellation readiness");
    };
    let (result, readiness) = tokio::join!(request, ready);
    readiness?;
    match result {
        Err(ChaosErr::Sandbox(SandboxErr::Timeout { output })) => {
            ensure!(output.timed_out && output.exit_code == 124);
            ensure!(output.stdout.text.contains("cancel-ready"));
        }
        other => bail!("unexpected cancellation result: {other:?}"),
    }
    Ok(())
}

async fn managed(
    helper: &Path,
    home: Arc<tempfile::TempDir>,
    command: &str,
    tty: bool,
    expected_code: i32,
    expected_output: &str,
) -> Result<()> {
    println!("managed fixture: tty={tty}, expected_exit={expected_code}");
    let helper = helper.to_path_buf();
    let builder = test_chaos().with_home(home).with_config(move |config| {
        config.alcatraz_exe = helper;
    });
    let harness = TestChaosHarness::with_builder(builder).await?;
    println!("managed fixture: isolated session ready");
    let call_id = "warden-managed";
    let args = serde_json::json!({
        "cmd": command,
        "login": false,
        "tty": tty,
        "yield_time_ms": 1000,
    });
    mount_sse_sequence(
        harness.server(),
        vec![
            sse(vec![
                ev_response_created("warden-1"),
                ev_function_call(call_id, "exec_command", &serde_json::to_string(&args)?),
                ev_completed("warden-1"),
            ]),
            sse(vec![
                ev_response_created("warden-2"),
                ev_assistant_message("warden-done", "local fixture complete"),
                ev_completed("warden-2"),
            ]),
        ],
    )
    .await;
    println!("managed fixture: loopback responses mounted");
    let test = harness.test();
    let policy = SandboxPolicy::new_read_only_policy();
    // The convenience submit_with_policy helper drains the completed turn.
    // Submit directly so this fixture can validate the actual exec events.
    test.process
        .submit(Op::UserTurn {
            items: vec![UserInput::Text {
                text: "execute the local fixture".into(),
                text_elements: Vec::new(),
            }],
            cwd: test.cwd_path().to_path_buf(),
            approval_policy: ApprovalPolicy::Headless,
            vfs_policy: VfsPolicy::from_sandbox_policy(&policy, test.cwd_path()),
            socket_policy: SocketPolicy::Restricted,
            model: test.session_configured.model.clone(),
            effort: None,
            summary: None,
            service_tier: None,
            final_output_json_schema: None,
            collaboration_mode: None,
            personality: None,
        })
        .await?;
    println!("managed fixture: turn submitted");
    let mut ended = None;
    let mut completed = false;
    while ended.is_none() || !completed {
        let event = harness.test().process.next_event().await?;
        match event.msg {
            EventMsg::ExecCommandEnd(end) if end.call_id == call_id => {
                ended = Some(end);
            }
            EventMsg::TurnComplete(_) => completed = true,
            EventMsg::Error(error) => bail!("managed fixture error: {error:?}"),
            _ => {}
        }
    }
    // Drain the completion response before assertions so mock verification
    // cannot mask a more useful execution-result failure during unwinding.
    let end = ended.context("missing managed completion")?;
    ensure!(end.exit_code == expected_code, "managed result: {end:?}");
    ensure!(
        end.aggregated_output.contains(expected_output),
        "missing managed output: {end:?}"
    );
    if tty {
        ensure!(!end.aggregated_output.contains("PTY-MUST-NOT-EXECUTE"));
    }
    let output = harness.function_call_stdout(call_id).await;
    ensure!(
        output.contains(expected_output),
        "missing tool-result content: {output}"
    );
    harness.test().process.shutdown_and_wait().await?;
    Ok(())
}

pub(super) async fn run(helper: &Path, home: Arc<tempfile::TempDir>) -> Result<()> {
    // Own the real journal server inside the fixture, never a detached daemon.
    let config = chaos_journald::JournalServerConfig::from_chaos_home(home.path());
    let client = chaos_journald::JournalRpcClient::new(config.socket_path.clone());
    let journal = tokio::spawn(chaos_journald::run_sqlite_journal_server(config));
    let result = async {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if client.hello("warden-fixture").await.is_ok() {
                    return Ok::<(), anyhow::Error>(());
                }
                ensure!(!journal.is_finished(), "fixture journal server exited");
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await??;
        run_cases(helper, home).await
    }
    .await;
    journal.abort();
    let _ = journal.await;
    result
}

async fn run_cases(helper: &Path, home: Arc<tempfile::TempDir>) -> Result<()> {
    let temp = tempfile::Builder::new()
        .prefix("chaos-warden-exec-")
        .tempdir_in("/tmp")?;
    let source = temp.path().join("probe.c");
    let probe = temp.path().join("probe");
    let marker = temp.path().join("must-not-write");
    std::fs::write(
        &source,
        include_str!("../../../../arch/freebsd/tests/fixtures/probe.c"),
    )?;
    ensure!(
        std::process::Command::new("cc")
            .args(["-Wall", "-Wextra", "-Werror", "-o"])
            .arg(&probe)
            .arg(&source)
            .status()?
            .success(),
        "probe compilation failed"
    );
    let probe = probe.to_str().context("non-UTF8 probe path")?;
    let marker_text = marker.to_str().context("non-UTF8 marker path")?;
    let output = oneshot(
        helper,
        temp.path(),
        vec![probe.into(), "warden-check".into(), marker_text.into()],
    )
    .await?;
    ensure!(output.exit_code == 0, "oneshot confinement: {output:?}");
    ensure!(output.stdout.text.contains("warden-check: jailed non-root"));
    ensure!(!marker.exists());
    let output = oneshot(
        helper,
        temp.path(),
        shell("printf 'one-out\\n'; printf 'one-err\\n' >&2; exit 37"),
    )
    .await?;
    ensure!(
        output.exit_code == 37
            && output.stdout.text == "one-out\n"
            && output.stderr.text == "one-err\n",
        "oneshot output/status: {output:?}"
    );
    let signal = oneshot(helper, temp.path(), shell("kill -TERM $$")).await;
    ensure!(
        matches!(signal, Err(ref error) if matches!(
            error.downcast_ref::<ChaosErr>(),
            Some(ChaosErr::Sandbox(SandboxErr::Signal(libc::SIGTERM)))
        )),
        "oneshot signal result: {signal:?}"
    );
    cancellation(helper, temp.path()).await?;
    // A successful next command also proves the serial worker recovered from
    // cancellation instead of retaining a busy command session.
    ensure!(
        oneshot(helper, temp.path(), shell("exit 0"))
            .await?
            .exit_code
            == 0
    );
    println!("oneshot: confinement, separate output, exit, signal, cancellation PASS");

    // Both paths are private tempfile names beneath a fixed /tmp prefix.
    ensure!(!probe.contains('\'') && !marker_text.contains('\''));
    managed(
        helper,
        home.clone(),
        &format!("'{probe}' warden-check '{marker_text}'"),
        false,
        0,
        "warden-check: jailed non-root",
    )
    .await?;
    ensure!(!marker.exists());
    managed(
        helper,
        home.clone(),
        "printf 'managed-out\\n'; exit 37",
        false,
        37,
        "managed-out",
    )
    .await?;
    // The existing cross-platform managed-process API uses -1 for a signaled
    // child; oneshot above separately checks the actual SIGTERM variant.
    managed(helper, home.clone(), "kill -TERM $$", false, -1, "").await?;
    managed(
        helper,
        home,
        "printf 'PTY-MUST-NOT-EXECUTE\\n'",
        true,
        125,
        "PTY",
    )
    .await?;
    println!("managed exec: real tool dispatch, confinement, exit, signal, PTY refusal PASS");
    Ok(())
}
