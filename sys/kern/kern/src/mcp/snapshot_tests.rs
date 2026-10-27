use super::*;

#[test]
fn snapshot_preserves_runtime_statuses_without_tools() {
    let statuses = HashMap::from([
        ("ready".to_string(), McpStartupStatus::Ready),
        (
            "failed".to_string(),
            McpStartupStatus::Failed {
                error: "executable not found".to_string(),
            },
        ),
    ]);
    let snapshot = build_mcp_snapshot(
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        HashMap::new(),
        statuses,
        HashMap::from([("ready".to_string(), serde_json::json!({"resources": {}}))]),
    );

    assert!(snapshot.tools.is_empty());
    assert!(snapshot.resources.is_empty());
    assert!(snapshot.resource_templates.is_empty());
    assert_eq!(
        snapshot.server_capabilities["ready"],
        serde_json::json!({"resources": {}})
    );
    std::assert_matches!(
        snapshot.server_statuses.get("ready"),
        Some(McpStartupStatus::Ready)
    );
    std::assert_matches!(
        snapshot.server_statuses.get("failed"),
        Some(McpStartupStatus::Failed { error }) if error == "executable not found"
    );
}
