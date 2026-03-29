use super::*;
use crate::builtin_mcp_resources;
use pretty_assertions::assert_eq;
use serde_json::json;

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
fn merge_inline_resources_adds_local_chaos_crons_resource() {
    let merged = merge_inline_resources(HashMap::new());

    let resources = merged
        .get(CHAOS_INLINE_SERVER_NAME)
        .expect("inline chaos resources should exist");

    assert_eq!(resources.len(), 2);
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
}

#[test]
fn inline_text_resource_result_wraps_json_text_content() {
    let result =
        inline_text_resource_result(builtin_mcp_resources::CHAOS_CRONS_URI, "[]".to_string());

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
fn merge_inline_resource_templates_adds_session_detail_template() {
    let merged = merge_inline_resource_templates(HashMap::new());

    let templates = merged
        .get(CHAOS_INLINE_SERVER_NAME)
        .expect("inline chaos templates should exist");

    assert_eq!(templates.len(), 1);
    assert_eq!(
        templates[0].uri_template,
        builtin_mcp_resources::CHAOS_SESSIONS_URI_TEMPLATE
    );
    assert_eq!(templates[0].name, "session_detail");
    assert_eq!(
        templates[0].description.as_deref(),
        Some("Details for a specific ChaOS process")
    );
}
