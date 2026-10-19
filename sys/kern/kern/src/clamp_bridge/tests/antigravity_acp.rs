//! Opt-in live integration with the actual kernel session bridge.
//! This does not enable a production ACP backend or attest sandbox/egress parity.
#![allow(clippy::expect_used, clippy::print_stderr)]

use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use acp_guest::protocol::schema::v1::{
    ContentBlock, EnvVariable, McpServer, McpServerStdio, NewSessionRequest, PermissionOptionKind,
    PromptRequest, RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionId, SessionNotification, SessionUpdate, StopReason,
    TextContent,
};
use acp_guest::{
    AcpClient, AcpConnection, ConnectionOptions, GuestError, PermissionHandler, protocol,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

// Reuse the bounded, credential-redacting operator-test helpers. These are
// source-only test modules, not dependencies of the production kernel.
#[path = "../../../../../../lib/libacp/guest/examples/support/tool_policy.rs"]
#[allow(dead_code)]
mod policy;
#[path = "../../../../../../lib/libacp/guest/examples/support/mod.rs"]
#[allow(dead_code)]
mod process;

#[path = "../../../../../../lib/libacp/guest/examples/support/hook.rs"]
mod hook;
use hook::Hook;

mod canonical_egress;

struct Bridge(Option<super::ClampSessionBridge>);

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(bridge) = self.0.take() {
            bridge.task.abort();
            let _ = std::fs::remove_file(bridge.socket_path);
        }
    }
}

#[derive(Debug)]
struct ReadPermission {
    file: String,
    session: Mutex<Option<SessionId>>,
    allowed: AtomicUsize,
    denied: AtomicUsize,
}

impl PermissionHandler for ReadPermission {
    fn request_permission(
        &self,
        request: RequestPermissionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<RequestPermissionResponse>> {
        let bound = self
            .session
            .lock()
            .ok()
            .is_some_and(|session| session.as_ref() == Some(&request.session_id));
        let meta = serde_json::to_value(&request.tool_call.meta).unwrap_or_default();
        let identity = request
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .is_some_and(|input| {
                policy::kernel_fixture_permission_identity(&meta, input, &self.file)
            })
            && request
                .tool_call
                .fields
                .name
                .as_deref()
                .is_none_or(|name| name == "read_file");
        let raw_fields: Vec<_> = request
            .tool_call
            .fields
            .raw_input
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|object| object.keys())
            .filter_map(|key| policy::safe_identifier(key))
            .take(16)
            .collect();
        let call_name = request
            .tool_call
            .fields
            .name
            .as_deref()
            .and_then(policy::safe_identifier);
        eprintln!(
            "Kernel MCP permission envelope: raw fields={raw_fields:?}, \
             optional tool name={call_name:?}"
        );
        let outcome = if bound && identity {
            request
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::AllowOnce)
                .map(|option| {
                    self.allowed.fetch_add(1, Ordering::Relaxed);
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                        option.option_id.clone(),
                    ))
                })
        } else {
            None
        };
        let outcome = outcome.unwrap_or_else(|| {
            self.denied.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "Kernel fixture permission denied: session_bound={bound}, exact_identity={identity}"
            );
            let raw = request
                .tool_call
                .fields
                .raw_input
                .as_ref()
                .unwrap_or(&Value::Null);
            let args = raw.get("arguments").unwrap_or(&Value::Null);
            let keys: Vec<_> = args
                .as_object()
                .into_iter()
                .flat_map(|object| object.keys())
                .filter_map(|key| policy::safe_identifier(key))
                .take(16)
                .collect();
            eprintln!(
                "Permission identity flags: mcp={}, server={}, tool={}, \
                 object_args={}, string_args={}, keys={keys:?}, \
                 exact_path={}, offset_one={}, limit_five={}",
                meta["is_mcp_tool_call"] == true,
                meta["mcp"]["server"] == "chaos",
                meta["mcp"]["tool"] == "read_file",
                args.is_object(),
                args.is_string(),
                args.get("file_path").and_then(Value::as_str) == Some(self.file.as_str()),
                args.get("offset").and_then(Value::as_f64) == Some(1.0),
                args.get("limit").and_then(Value::as_f64) == Some(5.0),
            );
            RequestPermissionOutcome::Cancelled
        });
        Box::pin(async move { Ok(RequestPermissionResponse::new(outcome)) })
    }
}

fn required<T>(result: Result<T, GuestError>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(GuestError::Protocol(error)) => panic!("{operation}: ACP code {}", error.code),
        Err(error) => panic!("{operation}: {error}"),
    }
}

async fn draining<R: protocol::JsonRpcRequest>(
    client: &AcpClient,
    updates: &mut mpsc::Receiver<SessionNotification>,
    request: R,
) -> Result<R::Response, GuestError> {
    let response = client.request(request);
    tokio::pin!(response);
    loop {
        tokio::select! {
            result = &mut response => return result,
            Some(_) = updates.recv() => {}
        }
    }
}

async fn prompt(
    client: &AcpClient,
    updates: &mut mpsc::Receiver<SessionNotification>,
    session: &SessionId,
    text: String,
) -> String {
    let response = client.request(PromptRequest::new(
        session.clone(),
        vec![ContentBlock::Text(TextContent::new(text))],
    ));
    tokio::pin!(response);
    let mut output = String::new();
    loop {
        tokio::select! {
            biased;
            Some(update) = updates.recv() => {
                if update.session_id == *session
                    && let SessionUpdate::AgentMessageChunk(chunk) = update.update
                    && let ContentBlock::Text(text) = chunk.content
                { output.push_str(&text.text); }
            }
            result = &mut response => {
                assert_eq!(required(result, "prompt").stop_reason, StopReason::EndTurn);
                return output;
            }
        }
    }
}

fn path_env(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("set {name}")));
    assert!(path.is_absolute(), "{name} must be absolute");
    path
}

fn marker() -> String {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .expect("OS randomness")
        .read_exact(&mut bytes)
        .expect("fixture entropy");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[ignore = "live AGY + actual Chaos bridge; dedicated OAuth profile \
            and explicit compiled binary paths required"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_agy_reads_fixture_through_real_chaos_session_bridge() {
    let server = path_env("CHAOS_AGY_ACP_PATH");
    let profile = path_env("CHAOS_AGY_ACP_HOME");
    let hook_executable = path_env("CHAOS_AGY_ACP_HOOK_PATH");
    let chaos_executable = path_env("CHAOS_AGY_ACP_CHAOS_PATH");
    assert!(server.with_file_name("localharness_external").is_file());
    assert!(hook_executable.is_file() && chaos_executable.is_file());

    let (_home, workspace, session) = super::session_bridge_fixture().await;
    let file = workspace.path().join("actual-kernel-fixture.txt");
    std::fs::write(&file, "").expect("create fixture");
    let file = std::fs::canonicalize(file).expect("canonical fixture");
    let hook = Hook::install(&profile, &hook_executable, &file);
    let mut bridge = Bridge(Some(
        super::ClampSessionBridge::spawn(Arc::downgrade(&session))
            .await
            .expect("actual session bridge"),
    ));
    let bridge_ref = bridge.0.as_ref().expect("bridge owned");
    let mcp = McpServerStdio::new("chaos", chaos_executable)
        .args(vec!["clamp-session-bridge".into()])
        .env(vec![
            EnvVariable::new(
                "CHAOS_CLAMP_MCP_SOCKET",
                bridge_ref.socket_path().to_string_lossy(),
            ),
            EnvVariable::new("CHAOS_CLAMP_MCP_TOKEN", bridge_ref.token()),
        ]);
    let permissions = Arc::new(ReadPermission {
        file: file.to_string_lossy().into_owned(),
        session: Mutex::new(None),
        allowed: AtomicUsize::new(0),
        denied: AtomicUsize::new(0),
    });
    let mut agent = process::Agent::spawn(&server, &profile).expect("official ACP runtime");
    let (connection, mut updates) = required(
        AcpConnection::connect(
            agent.child.stdout.take().expect("stdout"),
            agent.child.stdin.take().expect("stdin"),
            ConnectionOptions {
                request_timeout: Duration::from_secs(120),
                permissions: permissions.clone(),
                ..Default::default()
            },
        )
        .await,
        "initialize",
    );
    if let Some(info) = &connection.initialized().agent_info {
        eprintln!(
            "Actual kernel bridge live agent: {} {}",
            info.name, info.version
        );
    }
    let agent_workspace = tempfile::tempdir().expect("empty agent workspace");
    let client = connection.client();
    let provider = required(
        draining(
            &client,
            &mut updates,
            NewSessionRequest::new(agent_workspace.path()).mcp_servers(vec![McpServer::Stdio(mcp)]),
        )
        .await,
        "session/new with actual Chaos bridge",
    );
    *permissions.session.lock().expect("bind permissions") = Some(provider.session_id.clone());
    let expected = marker();
    std::fs::write(&file, &expected).expect("write unpredictable fixture");
    let output = prompt(
        &client,
        &mut updates,
        &provider.session_id,
        format!(
            "Use MCP server chaos tool read_file with exactly these arguments: {}. \
         Reply with exactly the marker in that file, without line numbering. \
         Do not use native tools.",
            json!({"file_path":file,"offset":1,"limit":5})
        ),
    )
    .await;
    let matched = output.trim() == expected;
    if !matched {
        let lower = output.to_ascii_lowercase();
        let categories: Vec<_> = [
            "permission",
            "arguments",
            "error",
            "failed",
            "unavailable",
            "schema",
            "timeout",
        ]
        .into_iter()
        .filter(|word| lower.contains(word))
        .collect();
        eprintln!("Kernel MCP final-answer diagnostic categories only: {categories:?}");
    }
    let continued = if matched {
        prompt(
            &client,
            &mut updates,
            &provider.session_id,
            "Without using tools, reply with exactly the marker from the previous turn.".into(),
        )
        .await
    } else {
        String::new()
    };
    let native_marker = marker();
    let native_file = agent_workspace.path().join("native-only.txt");
    std::fs::write(&native_file, &native_marker).expect("distinct native fixture");
    let native = if matched {
        prompt(
            &client,
            &mut updates,
            &provider.session_id,
            format!(
                "Now use your native view_file tool to read {}. Do not use MCP. \
             Reply with exactly its contents or NATIVE_DENIED if blocked.",
                native_file.display()
            ),
        )
        .await
    } else {
        String::new()
    };
    eprintln!(
        "Runtime diagnostic categories: {:?}",
        agent.diagnostic_categories()
    );
    connection.disconnect().await.expect("disconnect");
    agent.stop().await.expect("reap ACP/MCP runtime group");
    bridge
        .0
        .take()
        .expect("owned bridge")
        .shutdown()
        .await
        .expect("stop kernel bridge");
    let approvals = hook.observed("allowed", "read_file");
    let denials = hook.observed("denied", "view_file");
    let allowed = permissions.allowed.load(Ordering::Relaxed);
    let permission_denied = permissions.denied.load(Ordering::Relaxed);
    eprintln!(
        "Actual kernel fixture match: {matched}; hook read approvals: {approvals}; \
         native denials: {denials}; AllowOnce selections: {allowed}; \
         permission denials: {permission_denied}"
    );
    assert!(matched, "actual Chaos MCP fixture did not round-trip");
    assert!(
        continued.trim() == expected,
        "continued session lost the kernel fixture result"
    );
    assert!(
        !native.contains(&native_marker),
        "native fixture escaped the policy"
    );
    assert!(native.trim() == "NATIVE_DENIED");
    assert!(
        approvals > 0 && denials > 0,
        "missing hook enforcement evidence"
    );
    // Read-only MCP tools may not prompt. If a request is made, the policy
    // remains session-bound, exact-file and AllowOnce only.
    assert!(allowed <= 1, "unexpected repeated permission selection");
}
