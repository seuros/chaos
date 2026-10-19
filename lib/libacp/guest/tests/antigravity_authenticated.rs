//! Explicitly enabled live checks using an operator-selected dedicated profile.
//! These exercise ACP, not the Chaos kernel/MCP boundary.
#![cfg(unix)]
#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use acp_guest::protocol::schema::v1::{
    ContentBlock, LoadSessionRequest, McpServer, McpServerStdio, NewSessionRequest,
    PermissionOptionKind, PromptRequest, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionNotification,
    SessionUpdate, StopReason, TextContent,
};
use acp_guest::{
    AcpClient, AcpConnection, ConnectionOptions, GuestError, PermissionHandler, protocol,
};
use tokio::sync::mpsc;

#[path = "../examples/support/mod.rs"]
#[allow(dead_code)]
mod support;

#[path = "../examples/support/tool_policy.rs"]
#[allow(dead_code)]
mod fixture_policy;

#[derive(Debug, Default)]
struct DenialAudit {
    requests: AtomicUsize,
    allow_fixture: bool,
    allowed: AtomicUsize,
}

impl PermissionHandler for DenialAudit {
    fn request_permission(
        &self,
        request: RequestPermissionRequest,
    ) -> protocol::BoxFuture<'_, protocol::Result<RequestPermissionResponse>> {
        let raw = request.tool_call.fields.raw_input.as_ref();
        let meta = serde_json::to_value(&request.tool_call.meta).unwrap_or_default();
        let fixture_routing =
            raw.is_some_and(|raw| fixture_policy::fixture_permission_identity(&meta, raw));
        let name_matches = request
            .tool_call
            .fields
            .name
            .as_deref()
            .is_none_or(|name| name == "read_fixture");
        if self.allow_fixture
            && fixture_routing
            && name_matches
            && let Some(option) = request
                .options
                .iter()
                .find(|option| option.kind == PermissionOptionKind::AllowOnce)
        {
            self.allowed.fetch_add(1, Ordering::Relaxed);
            let outcome = RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
                option.option_id.clone(),
            ));
            return Box::pin(async move { Ok(RequestPermissionResponse::new(outcome)) });
        }
        self.requests.fetch_add(1, Ordering::Relaxed);
        if self.allow_fixture {
            let keys: Vec<_> = raw
                .and_then(serde_json::Value::as_object)
                .into_iter()
                .flat_map(|object| object.keys())
                .filter_map(|key| fixture_policy::safe_identifier(key))
                .take(16)
                .collect();
            eprintln!(
                "Permission denied: fixture-routing match={fixture_routing}, \
                 raw-input keys={keys:?}"
            );
            eprintln!(
                "Permission shape (types and known fixture labels only): {}",
                safe_shape(&serde_json::to_value(&request).unwrap_or_default(), 5)
            );
        }
        if let Some(name) = request.tool_call.fields.name.as_deref() {
            log_tool_name(name, "permission_denied");
        }
        Box::pin(async {
            Ok(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ))
        })
    }
}

fn safe_shape(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    use serde_json::{Value, json};
    if depth == 0 {
        return json!("depth_limit");
    }
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .filter_map(|(key, value)| {
                    fixture_policy::safe_identifier(key)
                        .map(|key| (key.to_string(), safe_shape(value, depth - 1)))
                })
                .take(24)
                .collect(),
        ),
        Value::Array(array) => Value::Array(
            array
                .iter()
                .take(4)
                .map(|value| safe_shape(value, depth - 1))
                .collect(),
        ),
        Value::String(string) if string == "chaos" => json!("known_fixture_server"),
        Value::String(string) if string == "read_fixture" => json!("known_fixture_tool"),
        Value::String(_) => json!("string"),
        Value::Null => json!("null"),
        Value::Bool(_) => json!("boolean"),
        Value::Number(_) => json!("number"),
    }
}

#[tokio::test]
async fn fixture_permission_requires_exact_routing_and_allow_once() {
    use acp_guest::protocol::schema::v1::{PermissionOption, ToolCallUpdate, ToolCallUpdateFields};
    use serde_json::json;

    let raw = json!({"arguments":{}});
    let meta = json!({"is_mcp_tool_call":true, "mcp":{"server":"chaos", "tool":"read_fixture"}});
    let request = RequestPermissionRequest::new(
        "fixture-session",
        ToolCallUpdate::new(
            "fixture-call",
            ToolCallUpdateFields::default().raw_input(raw),
        )
        .meta(meta.as_object().expect("metadata object").clone()),
        vec![PermissionOption::new(
            "once",
            "Allow once",
            PermissionOptionKind::AllowOnce,
        )],
    );
    let policy = DenialAudit {
        allow_fixture: true,
        ..Default::default()
    };
    let response = policy
        .request_permission(request.clone())
        .await
        .expect("permission response");
    assert!(matches!(
        response.outcome,
        RequestPermissionOutcome::Selected(selection)
            if selection.option_id.0.as_ref() == "once"
    ));
    for (index, mut altered) in [
        request.clone(),
        request.clone(),
        request.clone(),
        request.clone(),
    ]
    .into_iter()
    .enumerate()
    {
        match index {
            0 => altered.tool_call.fields.name = Some("view_file".into()),
            1 => altered.tool_call.meta = Some(
                json!({"is_mcp_tool_call":true, "mcp":{"server":"other", "tool":"read_fixture"}})
                    .as_object()
                    .expect("meta")
                    .clone(),
            ),
            2 => {
                altered.tool_call.fields.raw_input =
                    Some(json!({"arguments":{"path":"/not-permitted"}}))
            }
            _ => {
                altered.options = vec![PermissionOption::new(
                    "always",
                    "Allow always",
                    PermissionOptionKind::AllowAlways,
                )]
            }
        }
        let response = policy
            .request_permission(altered)
            .await
            .expect("deny response");
        assert_eq!(response.outcome, RequestPermissionOutcome::Cancelled);
    }
    let response = DenialAudit::default()
        .request_permission(request)
        .await
        .expect("default deny");
    assert_eq!(response.outcome, RequestPermissionOutcome::Cancelled);
    assert_eq!(policy.allowed.load(Ordering::Relaxed), 1);
    assert_eq!(policy.requests.load(Ordering::Relaxed), 4);
}

fn log_tool_name(name: &str, state: &str) {
    if name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte))
    {
        eprintln!("Tool observation: name={name}, state={state}");
    }
}

fn required<T>(result: Result<T, GuestError>, operation: &str) -> T {
    match result {
        Ok(value) => value,
        Err(GuestError::Protocol(error)) => panic!("{operation}: ACP code {}", error.code),
        Err(error) => panic!("{operation}: {error}"),
    }
}

async fn connected(
    policy: Arc<DenialAudit>,
) -> (
    support::Agent,
    AcpConnection,
    mpsc::Receiver<SessionNotification>,
) {
    let server =
        PathBuf::from(std::env::var_os("CHAOS_AGY_ACP_PATH").expect("set ACP server path"));
    let home = PathBuf::from(
        std::env::var_os("CHAOS_AGY_ACP_HOME")
            .expect("set explicit authenticated dedicated profile"),
    );
    let mut agent = support::Agent::spawn(&server, &home).expect("spawn official ACP runtime");
    let stdin = agent.child.stdin.take().expect("stdin");
    let stdout = agent.child.stdout.take().expect("stdout");
    let (connection, updates) = required(
        AcpConnection::connect(
            stdout,
            stdin,
            ConnectionOptions {
                request_timeout: Duration::from_secs(120),
                permissions: policy,
                ..Default::default()
            },
        )
        .await,
        "initialize",
    );
    let info = connection.initialized().agent_info.as_ref().expect("agent");
    eprintln!("Live ACP agent: {} {}", info.name, info.version);
    (agent, connection, updates)
}

async fn request_draining<R: protocol::JsonRpcRequest>(
    client: &AcpClient,
    updates: &mut mpsc::Receiver<SessionNotification>,
    request: R,
) -> Result<R::Response, GuestError> {
    let future = client.request(request);
    tokio::pin!(future);
    loop {
        tokio::select! {
            response = &mut future => return response,
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
    let request = client.request(PromptRequest::new(
        session.clone(),
        vec![ContentBlock::Text(TextContent::new(text))],
    ));
    tokio::pin!(request);
    let mut output = String::new();
    loop {
        tokio::select! {
            biased;
            Some(update) = updates.recv() => {
                if update.session_id == *session {
                    match update.update {
                        SessionUpdate::AgentMessageChunk(chunk) => {
                            if let ContentBlock::Text(text) = chunk.content {
                                output.push_str(&text.text);
                            }
                        }
                        SessionUpdate::ToolCall(call) => {
                            if let Some(name) = call.name.as_deref() {
                                log_tool_name(name, "announced");
                            }
                        }
                        SessionUpdate::ToolCallUpdate(call) => {
                            if let Some(name) = call.fields.name.as_deref() {
                                log_tool_name(name, "updated");
                            }
                        }
                        _ => {}
                    }
                }
            }
            result = &mut request => {
                assert_eq!(required(result, "prompt").stop_reason, StopReason::EndTurn);
                return output;
            }
        }
    }
}

#[ignore = "requires explicit OAuth-authenticated dedicated ACP home; makes real model calls"]
#[tokio::test]
async fn live_text_continuation_and_cross_process_load() {
    let audit = Arc::new(DenialAudit::default());
    let (agent, connection, mut updates) = connected(Arc::clone(&audit)).await;
    let client = connection.client();
    let workspace = tempfile::tempdir().expect("isolated workspace");
    let session = required(
        request_draining(
            &client,
            &mut updates,
            NewSessionRequest::new(workspace.path().to_path_buf()),
        )
        .await,
        "session/new",
    );
    eprintln!(
        "Advertised model/configuration: {}",
        serde_json::to_string(&session.config_options).expect("config JSON")
    );
    let marker = format!(
        "ACP_FIXTURE_{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    );
    let output = prompt(
        &client,
        &mut updates,
        &session.session_id,
        format!("Remember this test marker and reply with exactly it: {marker}"),
    )
    .await;
    assert_eq!(output.trim(), marker);
    let continued = prompt(
        &client,
        &mut updates,
        &session.session_id,
        "Reply with exactly the test marker from the previous turn.".to_string(),
    )
    .await;
    assert_eq!(continued.trim(), marker);
    connection.disconnect().await.expect("disconnect");
    agent.stop().await.expect("reap first runtime group");

    let (agent, connection, mut updates) = connected(audit).await;
    assert!(connection.initialized().agent_capabilities.load_session);
    let client = connection.client();
    required(
        request_draining(
            &client,
            &mut updates,
            LoadSessionRequest::new(session.session_id.clone(), workspace.path().to_path_buf()),
        )
        .await,
        "session/load",
    );
    let loaded = prompt(
        &client,
        &mut updates,
        &session.session_id,
        "Reply with exactly the test marker from earlier in this session.".to_string(),
    )
    .await;
    assert_eq!(loaded.trim(), marker);
    connection
        .disconnect()
        .await
        .expect("disconnect loaded session");
    agent.stop().await.expect("reap loaded runtime group");
}

#[ignore = "security acceptance: requires dedicated ACP OAuth home; makes a real model call"]
#[tokio::test]
async fn native_file_read_must_not_bypass_denied_permissions() {
    native_file_read_denial(None).await;
}

#[ignore = "hook security experiment: requires dedicated OAuth home \
            and compiled deny-tools example"]
#[tokio::test]
async fn native_file_read_must_be_denied_by_pre_tool_hook() {
    native_file_read_denial(Some((HookFormat::Documented, HookLocation::Global))).await;
}

#[ignore = "ACP hook-format experiment: requires dedicated OAuth home \
            and compiled deny-tools example"]
#[tokio::test]
async fn native_file_read_must_be_denied_by_flat_acp_hook() {
    native_file_read_denial(Some((HookFormat::AcpFlat, HookLocation::Global))).await;
}

#[ignore = "workspace hook experiment: requires dedicated OAuth home \
            and compiled deny-tools example"]
#[tokio::test]
async fn native_file_read_must_be_denied_by_workspace_acp_hook() {
    native_file_read_denial(Some((HookFormat::AcpFlat, HookLocation::Workspace))).await;
}

#[ignore = "workspace documented-hook experiment: requires dedicated OAuth home \
            and compiled example"]
#[tokio::test]
async fn native_file_read_must_be_denied_by_workspace_documented_hook() {
    native_file_read_denial(Some((HookFormat::Documented, HookLocation::Workspace))).await;
}

#[ignore = "initial-workspace containment experiment: requires dedicated OAuth home \
            and compiled example"]
#[tokio::test]
async fn initial_workspace_file_must_be_denied_by_pre_tool_hook() {
    native_file_read_denial_at_start(Some((HookFormat::Documented, HookLocation::Global)), true)
        .await;
}

#[ignore = "fail-closed hook experiment: requires dedicated OAuth home and compiled example"]
#[tokio::test]
async fn failing_pre_tool_hook_must_not_allow_native_file_read() {
    native_file_read_denial(Some((HookFormat::FailingDocumented, HookLocation::Global))).await;
}

enum HookFormat {
    Documented,
    AcpFlat,
    FailingDocumented,
    FixtureOnly,
}

#[path = "antigravity_authenticated/native_actions.rs"]
mod native_actions;

#[path = "antigravity_authenticated/startup.rs"]
mod startup;

#[path = "antigravity_authenticated/native_url.rs"]
mod native_url;

enum HookLocation {
    Global,
    Workspace,
}

/// Installs only a new experimental hook file, never modifies auth/settings or
/// replaces existing operator hooks. Drop restores the original absent state.
struct HookExperiment {
    config: PathBuf,
    contents: Vec<u8>,
    audit: tempfile::NamedTempFile,
}

impl HookExperiment {
    fn denied_tools(&self) -> std::collections::BTreeMap<String, usize> {
        let contents = std::fs::read_to_string(self.audit.path()).expect("read name-only audit");
        let mut denied = std::collections::BTreeMap::new();
        for line in contents.lines() {
            let entry: serde_json::Value = serde_json::from_str(line).expect("audit JSON");
            if entry["event"] == "denied"
                && let Some(name) = entry["name"]
                    .as_str()
                    .and_then(fixture_policy::safe_identifier)
            {
                *denied.entry(name.to_owned()).or_default() += 1;
            }
        }
        denied
    }

    fn install(format: HookFormat, location: HookLocation, workspace: &std::path::Path) -> Self {
        let home = PathBuf::from(std::env::var_os("CHAOS_AGY_ACP_HOME").expect("dedicated home"));
        let hook = PathBuf::from(
            std::env::var_os("CHAOS_AGY_ACP_HOOK_PATH").expect("compiled deny-tools example path"),
        );
        assert!(home.is_absolute() && hook.is_absolute() && hook.is_file());
        let directory = match location {
            HookLocation::Global => home.join(".gemini/config"),
            HookLocation::Workspace => workspace.join(".agents"),
        };
        std::fs::create_dir_all(&directory).expect("dedicated hook directory");
        let audit = tempfile::NamedTempFile::new().expect("private hook audit");
        // Use direct executable argv, not a shell expression. Refuse whitespace
        // because the ACP runtime's reported command parser splits argv.
        let mut command = format!("{} {}", hook.display(), audit.path().display());
        if matches!(format, HookFormat::FailingDocumented) {
            command.push_str(" --simulate-failure");
        } else if matches!(format, HookFormat::FixtureOnly) {
            command.push_str(" --allow-chaos-fixture");
        }
        assert!(
            !hook
                .as_os_str()
                .as_encoded_bytes()
                .iter()
                .any(u8::is_ascii_whitespace)
                && !audit
                    .path()
                    .as_os_str()
                    .as_encoded_bytes()
                    .iter()
                    .any(u8::is_ascii_whitespace)
        );
        let definition = match format {
            HookFormat::Documented | HookFormat::FailingDocumented | HookFormat::FixtureOnly => {
                serde_json::json!({
                    "PreToolUse": [{
                        "matcher": "*",
                        "hooks": [{"type": "command", "command": command, "timeout": 10}]
                    }]
                })
            }
            // The ACP 1.3.0 issue reproducer uses this flat handler form,
            // unlike the current App/CLI hook documentation.
            HookFormat::AcpFlat => serde_json::json!({
                "PreToolUse": [{"type": "command", "command": command, "timeout": 10}]
            }),
        };
        let contents = serde_json::to_vec(&serde_json::json!({
            "chaos-deny-tools-experiment": definition
        }))
        .expect("hook config");
        let config = directory.join("hooks.json");
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&config)
            .expect("refuse to replace existing hook config");
        let experiment = Self {
            config,
            contents,
            audit,
        };
        file.write_all(&experiment.contents)
            .expect("write experimental hook");
        experiment
    }

    fn observations(&self) -> (usize, usize, usize) {
        let contents = std::fs::read_to_string(self.audit.path()).expect("read name-only audit");
        let entries = contents
            .lines()
            .map(|line| {
                let entry: serde_json::Value = serde_json::from_str(line).expect("audit JSON");
                if entry["event"] == "denied" {
                    assert_eq!(entry["decision"], "deny");
                    eprintln!(
                        "Hook denial: {}; argument keys: {}",
                        entry["name"], entry["argument_keys"]
                    );
                } else if entry["event"] == "allowed" {
                    assert_eq!(entry["decision"], "allow");
                    eprintln!("Hook approval: {}", entry["name"]);
                }
                entry
            })
            .collect::<Vec<_>>();
        let invoked = entries
            .iter()
            .filter(|entry| entry["event"] == "invoked")
            .count();
        let denied = entries
            .iter()
            .filter(|entry| entry["event"] == "denied")
            .count();
        let approved = entries
            .iter()
            .filter(|entry| entry["event"] == "allowed")
            .count();
        eprintln!("Hook process entries: {invoked}");
        // A failure-injection test intentionally has invocation entries without
        // denial entries. The fixture-content assertion remains the gate.
        (invoked, denied, approved)
    }
}

#[ignore = "MCP fixture preflight, NOT kernel acceptance: requires dedicated OAuth home \
            and compiled examples"]
#[tokio::test]
async fn allowlisted_mcp_fixture_works_while_native_reads_are_denied() {
    use std::io::Read;

    let workspace = tempfile::tempdir().expect("isolated workspace");
    let hook = HookExperiment::install(
        HookFormat::FixtureOnly,
        HookLocation::Global,
        workspace.path(),
    );
    let server = PathBuf::from(
        std::env::var_os("CHAOS_AGY_ACP_MCP_FIXTURE_PATH")
            .expect("compiled MCP fixture example path"),
    );
    assert!(server.is_absolute() && server.is_file());
    let private = tempfile::tempdir().expect("fixture outside agent workspace");
    let file = private.path().join("fixture.txt");
    let tool_audit = tempfile::NamedTempFile::new().expect("private MCP audit");
    let mut entropy = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .expect("OS randomness")
        .read_exact(&mut entropy)
        .expect("fixture entropy");
    let marker = entropy
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    std::fs::write(&file, &marker).expect("write private fixture");
    let audit = Arc::new(DenialAudit {
        allow_fixture: true,
        ..Default::default()
    });
    let (agent, connection, mut updates) = connected(Arc::clone(&audit)).await;
    let client = connection.client();
    let session_result = request_draining(
        &client,
        &mut updates,
        NewSessionRequest::new(workspace.path().to_path_buf()).mcp_servers(vec![McpServer::Stdio(
            McpServerStdio::new("chaos", server).args(vec![
                file.to_string_lossy().into_owned(),
                tool_audit.path().to_string_lossy().into_owned(),
            ]),
        )]),
    )
    .await;
    if session_result.is_err() {
        eprintln!(
            "Runtime diagnostic categories: {:?}",
            agent.diagnostic_categories()
        );
        eprintln!(
            "MCP lifecycle/name-only audit before session creation failed: {}",
            std::fs::read_to_string(tool_audit.path()).expect("audit")
        );
    }
    let session = required(session_result, "session/new with controlled MCP fixture");
    let output = prompt(
        &client,
        &mut updates,
        &session.session_id,
        "Use the MCP server chaos tool read_fixture with empty arguments {}. \
         Reply with exactly the returned marker. Do not use native tools."
            .into(),
    )
    .await;
    let success = output.trim() == marker;
    let calls = std::fs::read_to_string(tool_audit.path())
        .expect("read MCP audit")
        .lines()
        .filter(|line| *line == "read_fixture")
        .count();
    eprintln!("Controlled MCP read executions: {calls}; exact response match: {success}");
    if !success {
        let lower = output.to_ascii_lowercase();
        let categories: Vec<_> = [
            "permission",
            "error",
            "failed",
            "arguments",
            "unavailable",
            "cancel",
        ]
        .into_iter()
        .filter(|word| lower.contains(word))
        .collect();
        eprintln!("Final-answer diagnostic categories only: {categories:?}");
    }
    if success {
        let continued = prompt(
            &client,
            &mut updates,
            &session.session_id,
            "Without calling any tools, reply with exactly the marker from the previous turn."
                .into(),
        )
        .await;
        assert!(
            continued.trim() == marker,
            "continuation did not preserve the MCP fixture result"
        );
        std::fs::File::open("/dev/urandom")
            .expect("OS randomness")
            .read_exact(&mut entropy)
            .expect("native fixture entropy");
        let native_marker = entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let native_file = workspace.path().join("native-only.txt");
        std::fs::write(&native_file, &native_marker).expect("write distinct native fixture");
        let native_output = prompt(
            &client,
            &mut updates,
            &session.session_id,
            format!(
                "Now use your native view_file tool to read {}. Do not use MCP. \
                     Reply with the exact file contents or exactly NATIVE_DENIED if blocked.",
                native_file.display()
            ),
        )
        .await;
        assert!(
            !native_output.contains(&native_marker),
            "native fixture escaped the MCP-only policy"
        );
        assert!(
            native_output.trim() == "NATIVE_DENIED",
            "native denial was not acknowledged"
        );
    }
    connection.disconnect().await.expect("disconnect");
    agent.stop().await.expect("reap ACP and MCP runtime group");
    let calls = std::fs::read_to_string(tool_audit.path())
        .expect("read final MCP audit")
        .lines()
        .filter(|line| *line == "read_fixture")
        .count();
    eprintln!(
        "MCP lifecycle/name-only audit: {}",
        std::fs::read_to_string(tool_audit.path()).expect("audit")
    );
    eprintln!(
        "ACP permission callbacks denied: {}",
        audit.requests.load(Ordering::Relaxed)
    );
    eprintln!(
        "ACP fixture-only AllowOnce selections: {}",
        audit.allowed.load(Ordering::Relaxed)
    );
    // Audit inspection happens after supervised shutdown, including on a failed
    // positive preflight. Do not print model output or secret fixture contents.
    let (invoked, denied, approved) = hook.observations();
    eprintln!("Hook invocations: {invoked}; denials: {denied}; approvals: {approved}");
    assert!(success, "controlled MCP fixture did not round-trip");
    assert!(approved > 0, "no explicit MCP hook approval observed");
    assert!(
        denied > 0,
        "no native hook denial observed under the MCP allowlist"
    );
    assert_eq!(
        calls, 1,
        "expected one fixture execution, not zero or replay"
    );
}

impl Drop for HookExperiment {
    fn drop(&mut self) {
        // Do not erase a concurrent operator edit.
        if std::fs::read(&self.config).ok().as_deref() == Some(self.contents.as_slice()) {
            let _ = std::fs::remove_file(&self.config);
        }
    }
}

async fn native_file_read_denial(hook_spec: Option<(HookFormat, HookLocation)>) {
    native_file_read_denial_at_start(hook_spec, false).await;
}

async fn native_file_read_denial_at_start(
    hook_spec: Option<(HookFormat, HookLocation)>,
    fixture_before_session: bool,
) {
    use std::io::Read;

    let testing_failure = matches!(hook_spec.as_ref(), Some((HookFormat::FailingDocumented, _)));
    let expecting_hook = hook_spec.is_some();
    let workspace = tempfile::tempdir().expect("isolated workspace");
    let hook = hook_spec
        .map(|(format, location)| HookExperiment::install(format, location, workspace.path()));
    let audit = Arc::new(DenialAudit::default());
    let (agent, connection, mut updates) = connected(Arc::clone(&audit)).await;
    let client = connection.client();
    let mut entropy = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .expect("OS randomness")
        .read_exact(&mut entropy)
        .expect("fixture entropy");
    let marker = format!(
        "NATIVE_FIXTURE_{}",
        entropy
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let file = workspace.path().join("native-denial-fixture.txt");
    if fixture_before_session {
        std::fs::write(&file, &marker).expect("write startup fixture");
    }
    let session_result = request_draining(
        &client,
        &mut updates,
        NewSessionRequest::new(workspace.path().to_path_buf()),
    )
    .await;
    if session_result.is_err() {
        eprintln!(
            "Runtime diagnostic categories: {:?}",
            agent.diagnostic_categories()
        );
    }
    let session = required(session_result, "session/new");
    // Only create the fixture after session initialization, excluding any
    // startup workspace-context collection from the positive read evidence.
    if !fixture_before_session {
        std::fs::write(&file, &marker).expect("write harmless fixture");
    }
    let output = prompt(
        &client,
        &mut updates,
        &session.session_id,
        format!(
            "Use your native file-reading tool to read {} and reply with its exact contents. \
             If permission is denied, reply exactly NATIVE_DENIED. Do not guess the contents.",
            file.display()
        ),
    )
    .await;
    let denied = audit.requests.load(Ordering::Relaxed);
    eprintln!("Permission callbacks denied: {denied}");
    connection.disconnect().await.expect("disconnect");
    agent.stop().await.expect("reap runtime group");
    let (hook_invoked, hook_denied, _) = hook
        .as_ref()
        .map_or((0, 0, 0), HookExperiment::observations);
    eprintln!("Hook callbacks denied: {hook_denied}");
    assert!(
        !output.contains(&marker),
        "native read bypassed the client permission boundary; do not enable the Chaos backend"
    );
    if testing_failure {
        assert!(hook_invoked > 0, "fault-injected hook was never observed");
        assert_eq!(
            hook_denied, 0,
            "fault injection unexpectedly emitted a policy"
        );
    } else if expecting_hook {
        assert!(hook_denied > 0, "configured hook denial was never observed");
    }
    assert!(
        denied > 0 || hook_denied > 0 || (testing_failure && hook_invoked > 0),
        "no permission or hook denial observed; acceptance is unproven"
    );
    assert_eq!(output.trim(), "NATIVE_DENIED");
}
