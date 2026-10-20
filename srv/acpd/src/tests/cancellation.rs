use super::*;
use chaos_ipc::permissions::{SocketPolicy, VfsPolicy};
use chaos_ipc::protocol::{ApprovalPolicy, SandboxPolicy};
use core_test_support::responses::ev_function_call;

/// Last-resort cleanup for this test's owned fixture, never a user process.
struct FixturePid(String);

impl FixturePid {
    fn alive(&self) -> bool {
        std::process::Command::new("/bin/kill")
            .args(["-0", &self.0])
            .output()
            .unwrap()
            .status
            .success()
    }
}

impl Drop for FixturePid {
    fn drop(&mut self) {
        if self.alive() {
            let _ = std::process::Command::new("/bin/kill")
                .args(["-TERM", &self.0])
                .output();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_joins_an_actual_native_terminal_before_fresh_prompt() {
    let mut fixture = Fixture::new().await;
    let pid_file = fixture.cwd.path().join("owned-fixture.pid");
    let command = format!("echo $$ > '{}'; exec /bin/sleep 120", pid_file.display());
    mount_sse_sequence(
        &fixture.server,
        vec![
            sse(vec![
                ev_response_created("exec-r1"),
                ev_function_call(
                    "owned-exec",
                    "exec_command",
                    &json!({
                        "cmd":command,"yield_time_ms":1000,"max_output_tokens":100
                    })
                    .to_string(),
                ),
                ev_completed("exec-r1"),
            ]),
            response("exec-r2", "fresh after terminal cancellation"),
        ],
    )
    .await;
    // This is a hermetic, explicitly unrestricted test fixture, not a
    // production policy override in the ACP adapter.
    fixture
        .config
        .permissions
        .approval_policy
        .set(ApprovalPolicy::Headless)
        .unwrap();
    fixture
        .config
        .permissions
        .sandbox_policy
        .set(SandboxPolicy::RootAccess)
        .unwrap();
    fixture.config.permissions.vfs_policy =
        VfsPolicy::from_sandbox_policy(&SandboxPolicy::RootAccess, fixture.cwd.path());
    fixture.config.permissions.socket_policy = SocketPolicy::from(&SandboxPolicy::RootAccess);
    let (mut peer, session) = fixture.peer().await;
    let first = peer
        .prompt(&session, "start the owned terminal fixture")
        .await;
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(value) = std::fs::read_to_string(&pid_file) {
                let value = value.trim().to_owned();
                if value.parse::<u32>().is_ok() {
                    break FixturePid(value);
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("native tool did not start the fixture");
    assert!(pid.alive());
    peer.cancel(&session).await;
    peer.finish(&session, first, "cancelled").await;
    assert!(
        !pid.alive(),
        "ACP cancelled response preceded terminal process exit"
    );
    let next = peer
        .prompt(&session, "fresh turn after actual interruption")
        .await;
    peer.finish(&session, next, "end_turn").await;
    peer.close().await;
}
