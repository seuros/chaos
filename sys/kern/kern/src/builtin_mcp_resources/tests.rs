use super::*;

#[test]
fn resolves_builtin_resource_uris() {
    assert_eq!(
        resolve_resource_uri(CHAOS_MACHINE_URI).expect("resolve machine"),
        Some(ResolvedChaosBuiltinResource::Machine)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_SESSIONS_URI).expect("resolve sessions"),
        Some(ResolvedChaosBuiltinResource::Sessions)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_CRONS_URI).expect("resolve crons"),
        Some(ResolvedChaosBuiltinResource::Crons)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_SPOOL_URI).expect("resolve spool"),
        Some(ResolvedChaosBuiltinResource::Spool)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_MODELS_URI).expect("resolve models"),
        Some(ResolvedChaosBuiltinResource::Models)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_MODES_URI).expect("resolve modes"),
        Some(ResolvedChaosBuiltinResource::Modes)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_MCP_URI).expect("resolve mcp"),
        Some(ResolvedChaosBuiltinResource::Mcp)
    );
    assert_eq!(
        resolve_resource_uri(CHAOS_MANUAL_URI).expect("resolve manual"),
        Some(ResolvedChaosBuiltinResource::ManualIndex)
    );
}

#[test]
fn resolves_session_detail_uri() {
    let process_id = ProcessId::default();
    let uri = format!("chaos://sessions/{process_id}");

    assert_eq!(
        resolve_resource_uri(&uri).expect("resolve session detail"),
        Some(ResolvedChaosBuiltinResource::SessionDetail { process_id })
    );
}

#[test]
fn rejects_invalid_session_detail_uri() {
    let err = resolve_resource_uri("chaos://sessions/not-a-uuid").expect_err("invalid uri");
    assert!(err.contains("invalid process_id"));
}

#[test]
fn resolves_manual_page_uri() {
    assert!(matches!(
        resolve_resource_uri("chaos://man/chaos-mcp.7").expect("resolve manual page"),
        Some(ResolvedChaosBuiltinResource::ManualPage(page)) if page.id == "chaos-mcp.7"
    ));
}

#[test]
fn mcp_status_json_is_sorted_and_does_not_serialize_secrets() {
    let alpha: McpServerConfig = serde_json::from_value(json!({
        "url": "https://example.com/mcp",
        "bearer_token": "secret"
    }))
    .expect("HTTP config");
    let beta: McpServerConfig = serde_json::from_value(json!({
        "command": "server",
        "env": { "API_TOKEN": "secret" },
        "enabled": false
    }))
    .expect("stdio config");
    let gamma: McpServerConfig = serde_json::from_value(json!({
        "command": "broken-server"
    }))
    .expect("failed stdio config");
    let servers = HashMap::from([
        ("beta".to_string(), beta),
        ("alpha".to_string(), alpha.clone()),
        ("gamma".to_string(), gamma.clone()),
    ]);
    let active = HashMap::from([("alpha".to_string(), alpha), ("gamma".to_string(), gamma)]);
    let auth = HashMap::from([("alpha".to_string(), McpAuthStatus::BearerToken)]);
    let startup_statuses = HashMap::from([
        ("alpha".to_string(), McpStartupStatus::Ready),
        (
            "gamma".to_string(),
            McpStartupStatus::Failed {
                error: "401 Unauthorized".to_string(),
            },
        ),
    ]);

    let text = mcp_json_from_servers(
        Some(3),
        &servers,
        Some(&active),
        &auth,
        Some(&startup_statuses),
    )
    .expect("MCP status JSON");
    assert!(!text.contains('\n'), "model-facing JSON must be compact");
    let value: serde_json::Value = serde_json::from_str(&text).expect("parse status JSON");

    assert_eq!(value["revision"], 3);
    assert_eq!(value["servers"][0]["name"], "alpha");
    assert_eq!(value["servers"][0]["auth_status"], "bearer_token");
    assert_eq!(value["servers"][0]["status"]["state"], "ready");
    assert_eq!(value["servers"][1]["name"], "beta");
    assert_eq!(value["servers"][1]["status"]["state"], "disabled");
    assert_eq!(value["servers"][2]["name"], "gamma");
    assert_eq!(value["servers"][2]["status"]["state"], "failed");
    assert_eq!(value["servers"][2]["status"]["error"], "401 Unauthorized");
    assert!(!text.contains("secret"));
    assert!(!text.contains("API_TOKEN"));
}

#[test]
fn mcp_status_is_unavailable_without_an_active_session() {
    let config: McpServerConfig = serde_json::from_value(json!({
        "command": "server"
    }))
    .expect("stdio config");
    let servers = HashMap::from([("server".to_string(), config)]);

    let text = mcp_json_from_servers(None, &servers, None, &HashMap::new(), None)
        .expect("MCP status JSON");
    let value: serde_json::Value = serde_json::from_str(&text).expect("parse status JSON");

    assert_eq!(value["servers"][0]["status"]["state"], "unavailable");
}
