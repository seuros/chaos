use super::*;
use crate::builtin_mcp_resources;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn resource_uri_templates_encode_bot_authors_and_optional_query_parameters() {
    for (parameters, expected) in [
        (
            json!({
                "state": "open",
                "author": "github-actions[bot]",
                "repo": "seuros/behavior_machines",
            }),
            "skipper://prs/open/github-actions%5Bbot%5D?repo=seuros%2Fbehavior_machines",
        ),
        (
            json!({"state": "open", "author": "dependabot[bot]"}),
            "skipper://prs/open/dependabot%5Bbot%5D",
        ),
        (
            json!({"state": "open", "author": "me", "repo": "owner/repo?x=1&y=#tag"}),
            "skipper://prs/open/me?repo=owner%2Frepo%3Fx%3D1%26y%3D%23tag",
        ),
    ] {
        let args: ResourceUriArgs = parse_args(Some(json!({
            "server": "skipper",
            "uri": "skipper://prs/{state}/{author}{?repo}",
            "parameters": parameters,
        })))
        .expect("resource arguments");
        assert_eq!(args.server.as_deref(), Some("skipper"));
        assert_eq!(
            resolve_resource_uri(args.uri, args.parameters).expect("expanded URI"),
            expected,
        );
    }
}

#[test]
fn concrete_resource_uris_without_parameters_are_not_reencoded() {
    for uri in [
        "skipper://prs/open/github-actions%5Bbot%5D?repo=seuros/behavior_machines",
        "memo://host/a+b?x=1&y=2",
        "tasks://result/opaque/id",
    ] {
        let args: ResourceUriArgs = parse_args(Some(json!({"uri": uri}))).unwrap();
        assert_eq!(
            resolve_resource_uri(args.uri, args.parameters).unwrap(),
            uri
        );
    }
}

#[test]
fn resource_uri_templates_reject_invalid_templates_and_non_string_parameters() {
    for uri in ["memo://host/{", "memo://host/{}", "relative/{name}"] {
        assert!(resolve_resource_uri(uri.into(), Some(HashMap::new())).is_err());
    }
    assert!(resolve_resource_uri(" \t".into(), None).is_err());
    for value in [json!(1), json!(false), json!([]), Value::Null] {
        let arguments = Some(json!({
            "uri": "memo://host/{name}",
            "parameters": {"name": value},
            "subscribed": true,
        }));
        assert!(parse_args::<ResourceUriArgs>(arguments.clone()).is_err());
        assert!(parse_args::<ResourceSubscriptionArgs>(arguments).is_err());
    }
}

fn resource(uri: &str, name: &str) -> ResourceInfo {
    ResourceInfo {
        uri: uri.to_string(),
        name: name.to_string(),
        title: None,
        description: None,
        mime_type: None,
        size: None,
        icons: None,
        annotations: None,
        meta: None,
    }
}

fn template(uri_template: &str, name: &str) -> ResourceTemplateInfo {
    ResourceTemplateInfo {
        uri_template: uri_template.to_string(),
        name: name.to_string(),
        title: None,
        description: None,
        mime_type: None,
        icons: None,
        annotations: None,
        meta: None,
    }
}

#[test]
fn resource_with_server_serializes_server_field() {
    let entry = ResourceWithServer::new("test".to_string(), resource("memo://id", "memo"));
    let value = serde_json::to_value(&entry).expect("serialize resource");

    assert_eq!(value["server"], json!("test"));
    assert_eq!(value["uri"], json!("memo://id"));
    assert_eq!(value["name"], json!("memo"));
}

#[test]
fn list_resources_payload_from_single_server_copies_next_cursor() {
    let result = ListResourcesResult {
        meta: None,
        next_cursor: Some("cursor-1".to_string()),
        resources: vec![resource("memo://id", "memo")],
    };
    let payload = ListResourcesPayload::from_single_server("srv".to_string(), result);
    let value = serde_json::to_value(&payload).expect("serialize payload");

    assert_eq!(value["server"], json!("srv"));
    assert_eq!(value["nextCursor"], json!("cursor-1"));
    let resources = value["resources"].as_array().expect("resources array");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["server"], json!("srv"));
}

#[test]
fn list_resources_payload_from_all_servers_is_sorted() {
    let mut map = HashMap::new();
    map.insert("beta".to_string(), vec![resource("memo://b-1", "b-1")]);
    map.insert(
        "alpha".to_string(),
        vec![resource("memo://a-1", "a-1"), resource("memo://a-2", "a-2")],
    );

    let payload = ListResourcesPayload::from_all_servers(map);
    let value = serde_json::to_value(&payload).expect("serialize payload");
    let uris: Vec<String> = value["resources"]
        .as_array()
        .expect("resources array")
        .iter()
        .map(|entry| entry["uri"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        uris,
        vec![
            "memo://a-1".to_string(),
            "memo://a-2".to_string(),
            "memo://b-1".to_string()
        ]
    );
}

#[test]
fn call_tool_result_from_content_marks_success() {
    let result = call_tool_result_from_content("{}", Some(true));
    assert_eq!(result.is_error, Some(false));
    assert_eq!(result.content.len(), 1);
}

#[test]
fn resource_subscription_payload_reports_current_state() {
    let subscribed = ResourceSubscriptionPayload {
        server: "srv".to_string(),
        uri: "memo://id".to_string(),
        subscribed: true,
    };
    let unsubscribed = ResourceSubscriptionPayload {
        subscribed: false,
        ..subscribed
    };

    assert_eq!(
        serde_json::to_value(ResourceSubscriptionResponse::Mcp(unsubscribed))
            .expect("serialize subscription payload"),
        json!({
            "server": "srv",
            "uri": "memo://id",
            "subscribed": false
        })
    );
}

#[tokio::test]
async fn internal_subscription_falls_back_to_snapshot_and_unsubscribe_is_noop() {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let uri = "chaos://sessions";
    let expected = read_resource_contents(&session, &turn, None, uri)
        .await
        .expect("read internal resource");
    for (uri_argument, parameters, subscribed) in [
        (uri, Value::Null, true),
        (uri, Value::Null, false),
        ("chaos://{resource}", json!({"resource": "sessions"}), true),
        ("chaos://{resource}", json!({"resource": "sessions"}), false),
    ] {
        let output = handle_resource_subscription(
            session.clone(),
            turn.clone(),
            "subscription-fallback".to_string(),
            Some(json!({
                "uri": uri_argument,
                "parameters": parameters,
                "subscribed": subscribed
            })),
        )
        .await
        .expect("internal subscription should degrade gracefully");
        assert_eq!(output.success, Some(true));
        let text = function_call_output_content_items_to_text(&output.body).expect("text output");
        let value: Value = serde_json::from_str(&text).expect("JSON payload");
        assert!(value.get("server").is_none());
        assert_eq!(value["uri"], uri);
        assert_eq!(value["subscribed"], false);
        assert_eq!(value["subscription_supported"], false);
        assert!(
            value["message"]
                .as_str()
                .unwrap()
                .contains("No subscription is active")
        );
        if subscribed {
            assert_eq!(value["fallback"], "read_once");
            assert_eq!(value["snapshot"], serde_json::to_value(&expected).unwrap());
        } else {
            assert_eq!(value["fallback"], "no_op");
            assert!(value.get("snapshot").is_none());
        }
    }
}

#[tokio::test]
async fn subscription_fallback_does_not_hide_invalid_resources_or_external_server_errors() {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    for (server, uri) in [
        (None, "chaos://not-a-resource"),
        (Some("not-configured"), "memo://unknown"),
        (Some("not-configured"), "chaos://sessions"),
    ] {
        let result = handle_resource_subscription(
            session.clone(),
            turn.clone(),
            "subscription-error".to_string(),
            Some(json!({"server": server, "uri": uri, "subscribed": true})),
        )
        .await;
        assert!(result.is_err(), "must not mask errors for {server:?}/{uri}");
    }
}

#[test]
fn resource_routing_requires_omitted_server_for_internal_resources() {
    assert_eq!(normalize_resource_server(None).unwrap(), None);
    for server in ["chaos", "local", "guessed-server"] {
        assert_eq!(
            normalize_resource_server(Some(server.into())).unwrap(),
            Some(server.into())
        );
    }
    let error = normalize_resource_server(Some("chaos_local".into())).unwrap_err();
    assert!(error.to_string().contains("omit server"));
    assert!(normalize_resource_server(Some(" ".into())).is_err());
}

#[tokio::test]
async fn aggregate_listings_omit_server_only_for_internal_entries() {
    let (session, _) = crate::chaos::make_session_and_context().await;
    let resources = merge_chaos_resources(
        HashMap::from([("external".into(), vec![resource("memo://id", "memo")])]),
        &session,
    );
    let value = serde_json::to_value(resources).unwrap();
    let entries = value["resources"].as_array().unwrap();
    assert_eq!(entries[0]["server"], "external");
    assert!(
        entries
            .iter()
            .skip(1)
            .all(|entry| entry.get("server").is_none())
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry["uri"] == "chaos://sessions")
    );

    let templates = merge_chaos_resource_templates(
        HashMap::from([("external".into(), vec![template("memo://{id}", "memo")])]),
        &session,
    );
    let value = serde_json::to_value(templates).unwrap();
    let entries = value["resourceTemplates"].as_array().unwrap();
    assert_eq!(entries[0]["server"], "external");
    assert!(entries.len() > 1);
    assert!(
        entries
            .iter()
            .skip(1)
            .all(|entry| entry.get("server").is_none())
    );
}

#[tokio::test]
async fn serverless_task_resources_list_poll_and_read_results() {
    use mcp_host::protocol::types::TaskStatus;
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let task = session
        .services
        .internal_task_store
        .create_task(
            None,
            TaskStatus::Completed,
            None,
            Some(json!({"output": "done"})),
            None,
        )
        .await;
    for uri in [
        "tasks://".to_string(),
        format!("tasks://get/{}", task.task_id),
        format!("tasks://result/{}", task.task_id),
    ] {
        let output = handle_read_resource(
            session.clone(),
            turn.clone(),
            "task-read".into(),
            Some(json!({"uri": uri})),
        )
        .await
        .unwrap();
        let text = function_call_output_content_items_to_text(&output.body).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert!(value.get("server").is_none());
        let result: Value =
            serde_json::from_str(value["contents"][0]["text"].as_str().unwrap()).unwrap();
        if uri == "tasks://" {
            assert_eq!(result["tasks"][0]["taskId"], task.task_id);
        } else if uri.starts_with("tasks://get/") {
            assert_eq!(result["taskId"], task.task_id);
        } else {
            assert_eq!(result["output"], "done");
        }
    }
    assert!(
        read_resource_contents(&session, &turn, None, "tasks://get/missing")
            .await
            .is_err()
    );
    assert!(
        read_resource_contents(&session, &turn, Some("not-configured"), "tasks://")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn internal_resources_require_serverless_reads() {
    let (session, turn) = crate::chaos::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    for server in ["chaos_local", "chaos", "local", "guessed-server"] {
        for arguments in [
            json!({"server": server, "uri": "chaos://sessions"}),
            json!({
                "server": server,
                "uri": "chaos://{resource}",
                "parameters": {"resource": "sessions"},
            }),
        ] {
            let result = handle_read_resource(
                session.clone(),
                turn.clone(),
                "global-read".into(),
                Some(arguments),
            )
            .await;
            assert!(result.is_err(), "must not redirect {server}");
        }
    }
    for arguments in [
        json!({"uri": "chaos://sessions"}),
        json!({"uri": "chaos://{resource}", "parameters": {"resource": "sessions"}}),
    ] {
        let output = handle_read_resource(
            session.clone(),
            turn.clone(),
            "internal-read".into(),
            Some(arguments),
        )
        .await
        .expect("serverless internal read");
        let text = function_call_output_content_items_to_text(&output.body).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(output.success, Some(true));
        assert!(value.get("server").is_none());
        assert_eq!(value["uri"], "chaos://sessions");
        assert!(value["contents"].is_array());
        assert!(value.get("snapshot").is_none());
    }
}

#[test]
fn parse_arguments_handles_empty_and_json() {
    assert!(
        parse_arguments(" \n\t").unwrap().is_none(),
        "expected None for empty arguments"
    );

    assert!(
        parse_arguments("null").unwrap().is_none(),
        "expected None for null arguments"
    );

    let value = parse_arguments(r#"{"server":"figma"}"#)
        .expect("parse json")
        .expect("value present");
    assert_eq!(value["server"], json!("figma"));
}

#[test]
fn template_with_server_serializes_server_field() {
    let entry = ResourceTemplateWithServer::new("srv".to_string(), template("memo://{id}", "memo"));
    let value = serde_json::to_value(&entry).expect("serialize template");

    assert_eq!(
        value,
        json!({
            "server": "srv",
            "uriTemplate": "memo://{id}",
            "name": "memo"
        })
    );
}

#[test]
fn inline_resources_include_builtin_resources() {
    let resources = chaos_inline_resources();

    assert_eq!(resources.len(), 9);
    assert_eq!(resources[8].uri, builtin_mcp_resources::CHAOS_HOOKS_URI);
    let machine = resources
        .iter()
        .find(|resource| resource.uri == builtin_mcp_resources::CHAOS_MACHINE_URI)
        .expect("machine resource is discoverable");
    assert_eq!(machine.name, "machine");
    assert_eq!(
        machine.mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[0].uri, builtin_mcp_resources::CHAOS_SESSIONS_URI);
    assert_eq!(resources[0].name, "sessions");
    assert_eq!(
        resources[0].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[1].uri, builtin_mcp_resources::CHAOS_CRONS_URI);
    assert_eq!(resources[1].name, "crons");
    assert_eq!(
        resources[1].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[2].uri, builtin_mcp_resources::CHAOS_SPOOL_URI);
    assert_eq!(resources[2].name, "spool");
    assert_eq!(
        resources[2].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[3].uri, builtin_mcp_resources::CHAOS_MODELS_URI);
    assert_eq!(resources[3].name, "models");
    assert_eq!(
        resources[3].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[4].uri, builtin_mcp_resources::CHAOS_MODES_URI);
    assert_eq!(resources[4].name, "modes");
    assert_eq!(
        resources[4].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[5].uri, builtin_mcp_resources::CHAOS_MCP_URI);
    assert_eq!(resources[5].name, "mcp");
    assert_eq!(
        resources[5].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
    assert_eq!(resources[6].uri, builtin_mcp_resources::CHAOS_MANUAL_URI);
    assert_eq!(resources[6].name, "manual");
    assert_eq!(
        resources[6].mime_type.as_deref(),
        Some(builtin_mcp_resources::JSON_MIME_TYPE)
    );
}

#[test]
fn inline_text_resource_result_wraps_json_text_content() {
    let result = inline_text_resource_result(
        builtin_mcp_resources::CHAOS_CRONS_URI,
        "[]".to_string(),
        builtin_mcp_resources::JSON_MIME_TYPE,
    );

    assert_eq!(
        result,
        ReadResourceResult {
            contents: vec![ResourceContents::Text(ResourceContentsText {
                uri: builtin_mcp_resources::CHAOS_CRONS_URI.to_string(),
                mime_type: Some(builtin_mcp_resources::JSON_MIME_TYPE.to_string()),
                text: "[]".to_string(),
                meta: None,
            })],
            meta: None,
        }
    );
}

#[test]
fn inline_resource_templates_include_builtin_templates() {
    let templates = chaos_inline_resource_templates();

    assert_eq!(templates.len(), 3);
    assert_eq!(
        templates[2].uri_template,
        builtin_mcp_resources::CHAOS_HOOKS_URI_TEMPLATE
    );
    assert_eq!(
        templates[0].uri_template,
        builtin_mcp_resources::CHAOS_SESSIONS_URI_TEMPLATE
    );
    assert_eq!(templates[0].name, "session_detail");
    assert_eq!(
        templates[0].description.as_deref(),
        Some("Details for a specific ChaOS process")
    );
    assert_eq!(
        templates[1].uri_template,
        builtin_mcp_resources::CHAOS_MANUAL_URI_TEMPLATE
    );
    assert_eq!(templates[1].name, "manual_page");
    assert_eq!(
        templates[1].mime_type.as_deref(),
        Some(builtin_mcp_resources::MARKDOWN_MIME_TYPE)
    );
}

#[test]
fn inline_text_resource_result_preserves_markdown_mime_type() {
    let result = inline_text_resource_result(
        "chaos://man/chaos-mcp.7",
        "# chaos-mcp(7)".to_string(),
        builtin_mcp_resources::MARKDOWN_MIME_TYPE,
    );

    let ResourceContents::Text(content) = &result.contents[0] else {
        panic!("expected text resource");
    };
    assert_eq!(
        content.mime_type.as_deref(),
        Some(builtin_mcp_resources::MARKDOWN_MIME_TYPE)
    );
}
