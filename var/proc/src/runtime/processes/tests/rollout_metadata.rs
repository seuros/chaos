use super::*;
use chaos_ipc::protocol::CompactedItem;
use pretty_assertions::assert_eq;

fn meta(tools: Option<Vec<DynamicToolSpec>>, memory_mode: Option<&str>) -> RolloutItem {
    RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            id: ProcessId::new(),
            forked_from_id: None,
            timestamp: jiff::Timestamp::UNIX_EPOCH.to_string(),
            cwd: PathBuf::new(),
            originator: String::new(),
            cli_version: String::new(),
            source: SessionSource::Cli,
            agent_nickname: None,
            agent_role: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: tools,
            memory_mode: memory_mode.map(str::to_string),
            protected_context: None,
        },
        git: None,
    })
}

fn compacted() -> RolloutItem {
    RolloutItem::Compacted(CompactedItem {
        message: String::new(),
        replacement_history: Vec::new(),
    })
}

#[test]
fn dynamic_tools_use_first_metadata_and_distinguish_absent_unset_and_empty() {
    assert_eq!(extract_dynamic_tools(&[]), None);
    assert_eq!(extract_dynamic_tools(&[compacted()]), None);
    assert_eq!(extract_dynamic_tools(&[meta(None, None)]), Some(None));
    assert_eq!(
        extract_dynamic_tools(&[compacted(), meta(Some(Vec::new()), None), meta(None, None)]),
        Some(Some(Vec::new()))
    );
    let tools = vec![DynamicToolSpec {
        name: "lookup".into(),
        description: "look up a value".into(),
        input_schema: serde_json::json!({"type": "object"}),
        defer_loading: false,
    }];
    assert_eq!(
        extract_dynamic_tools(&[meta(None, None), meta(Some(tools.clone()), None)]),
        Some(None)
    );
    assert_eq!(
        extract_dynamic_tools(&[meta(Some(tools.clone()), None), meta(None, None)]),
        Some(Some(tools))
    );
}

#[test]
fn memory_mode_uses_latest_explicit_value_and_skips_unset_metadata() {
    assert_eq!(extract_memory_mode(&[]), None);
    assert_eq!(extract_memory_mode(&[compacted(), meta(None, None)]), None);
    assert_eq!(
        extract_memory_mode(&[
            meta(None, Some("enabled")),
            compacted(),
            meta(None, Some("disabled")),
            meta(None, None),
        ]),
        Some("disabled".into())
    );
}
