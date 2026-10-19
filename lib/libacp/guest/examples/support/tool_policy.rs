//! Experimental, deliberately fixed allowlist for a single MCP fixture.
//! Production Chaos tool identity and authorization are not implemented here.
use serde_json::Value;

pub fn decision(request: &Value, allow_fixture: bool) -> &'static str {
    if allow_fixture
        // ACP 1.4.0 reported the MCP leaf name here, not "call_mcp_tool".
        && request.pointer("/toolCall/name").and_then(Value::as_str) == Some("read_fixture")
        && request.pointer("/toolCall/args").is_some_and(fixture_arguments)
    {
        // Only the controlled fixture is approved here. A real bridge must
        // still enforce the kernel's own policy for every actual tool call.
        // Preserve AGY's default permission policy as well as the fixture's
        // own validation; this is not a grant for any other action.
        "allow"
    } else {
        "deny"
    }
}

pub fn fixture_arguments(arguments: &Value) -> bool {
    arguments
        .as_object()
        .is_some_and(|object| object.len() == 3)
        && arguments.get("ServerName").and_then(Value::as_str) == Some("chaos")
        && arguments.get("ToolName").and_then(Value::as_str) == Some("read_fixture")
        && arguments
            .get("Arguments")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
}

pub fn kernel_fixture_arguments(arguments: &Value, file: &str) -> bool {
    arguments
        .as_object()
        .is_some_and(|object| object.len() == 3)
        && arguments.get("file_path").and_then(Value::as_str) == Some(file)
        && arguments.get("offset").and_then(Value::as_u64) == Some(1)
        && arguments.get("limit").and_then(Value::as_u64) == Some(5)
}

pub fn kernel_fixture_decision(request: &Value, file: &str) -> &'static str {
    if request.pointer("/toolCall/name").and_then(Value::as_str) == Some("read_file")
        && request
            .pointer("/toolCall/args/ServerName")
            .and_then(Value::as_str)
            == Some("chaos")
        && request
            .pointer("/toolCall/args/ToolName")
            .and_then(Value::as_str)
            == Some("read_file")
        && request
            .pointer("/toolCall/args/Arguments")
            .is_some_and(|arguments| kernel_fixture_arguments(arguments, file))
    {
        "allow"
    } else {
        "deny"
    }
}

#[cfg(test)]
pub fn kernel_fixture_permission_identity(meta: &Value, input: &Value, file: &str) -> bool {
    meta.get("is_mcp_tool_call").and_then(Value::as_bool) == Some(true)
        && meta.pointer("/mcp/server").and_then(Value::as_str) == Some("chaos")
        && meta.pointer("/mcp/tool").and_then(Value::as_str) == Some("read_file")
        // rawInput is display metadata and may include other top-level fields.
        // The actual nested arguments must still match the complete fixed read.
        && input.get("arguments").is_some_and(|arguments| {
            arguments.as_object().is_some_and(|object| object.len() == 3)
                && arguments.get("file_path").and_then(Value::as_str) == Some(file)
                // Permission UI metadata may round-trip JSON integers through
                // floating-point values. These two exact small values are
                // equivalent; strings, fractions and other values stay denied.
                && arguments.get("offset").and_then(Value::as_f64) == Some(1.0)
                && arguments.get("limit").and_then(Value::as_f64) == Some(5.0)
        })
}

#[cfg(test)]
pub fn fixture_permission_identity(meta: &Value, input: &Value) -> bool {
    // Observed ACP 1.4.0 extension, used only in this opt-in fixture test.
    // Not a stable ACP-wide contract or a production authorization API.
    meta.get("is_mcp_tool_call").and_then(Value::as_bool) == Some(true)
        && meta.pointer("/mcp/server").and_then(Value::as_str) == Some("chaos")
        && meta.pointer("/mcp/tool").and_then(Value::as_str) == Some("read_fixture")
        && input.as_object().is_some_and(|object| object.len() == 1)
        && input
            .get("arguments")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
}

pub fn safe_identifier(value: &str) -> Option<&str> {
    (!value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./-".contains(&byte)))
    .then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kernel_read_is_limited_to_exact_file_and_arguments() {
        let file = "/test/kernel-fixture.txt";
        let arguments = json!({"file_path":file,"offset":1,"limit":5});
        let request = json!({"toolCall":{"name":"read_file","args":{
            "ServerName":"chaos","ToolName":"read_file","Arguments":arguments
        }}});
        assert_eq!(kernel_fixture_decision(&request, file), "allow");
        assert_eq!(kernel_fixture_decision(&request, "/other"), "deny");
        let mut native = request;
        native["toolCall"]["name"] = json!("view_file");
        assert_eq!(kernel_fixture_decision(&native, file), "deny");
        let meta = json!({"is_mcp_tool_call":true,"mcp":{"server":"chaos","tool":"read_file"}});
        assert!(kernel_fixture_permission_identity(
            &meta,
            &json!({"arguments":arguments}),
            file
        ));
        assert!(kernel_fixture_permission_identity(
            &meta,
            &json!({
                "arguments": arguments,
                "display_note": "UI metadata is not execution arguments"
            }),
            file
        ));
        assert!(kernel_fixture_permission_identity(
            &meta,
            &json!({"arguments":{"file_path":file,"offset":1.0,"limit":5.0}}),
            file
        ));
        assert!(!kernel_fixture_permission_identity(
            &meta,
            &json!({"arguments":{"file_path":file,"offset":1.5,"limit":5.0}}),
            file
        ));
        assert!(!kernel_fixture_permission_identity(
            &meta,
            &json!({"arguments":{}}),
            file
        ));
        let mut extra = arguments;
        extra["mode"] = json!("slice");
        assert!(!kernel_fixture_arguments(&extra, file));
    }

    #[test]
    fn fixture_requires_explicit_opt_in_and_exact_identity() {
        let request = json!({"toolCall": {
            "name": "read_fixture",
            "args": {"ServerName": "chaos", "ToolName": "read_fixture", "Arguments": {}}
        }});
        assert_eq!(decision(&request, false), "deny");
        assert_eq!(decision(&request, true), "allow");
        for (pointer, replacement) in [
            ("/toolCall/name", json!("view_file")),
            ("/toolCall/name", json!("call_mcp_tool")),
            ("/toolCall/name", json!("mcp__chaos__read_fixture")),
            ("/toolCall/args/ServerName", json!("other")),
            ("/toolCall/args/ServerName", json!("chaos-extra")),
            ("/toolCall/args/ToolName", json!("read_file")),
            ("/toolCall/args/ToolName", json!("read_fixture_extra")),
            ("/toolCall/args/ToolName", json!(null)),
            (
                "/toolCall/args/Arguments",
                json!({"file_path": "/not-permitted"}),
            ),
            ("/toolCall/args/Arguments", json!("{}")),
            ("/toolCall/args", json!("not-an-object")),
        ] {
            let mut altered = request.clone();
            if let Some(value) = altered.pointer_mut(pointer) {
                *value = replacement;
            }
            assert_eq!(decision(&altered, true), "deny");
        }
    }

    #[test]
    fn permission_routing_rejects_missing_identity_and_extra_envelope_fields() {
        let meta = json!({"is_mcp_tool_call":true,"mcp":{"server":"chaos","tool":"read_fixture"}});
        assert!(fixture_permission_identity(&meta, &json!({"arguments":{}})));
        for input in [
            json!({"arguments":"{}"}),
            json!({"arguments":{}, "extra":true}),
            json!({"arguments":{"file_path":"/not-permitted"}}),
            json!({}),
        ] {
            assert!(!fixture_permission_identity(&meta, &input));
        }
        for altered in [
            json!({}),
            json!({"is_mcp_tool_call":false,"mcp":{"server":"chaos","tool":"read_fixture"}}),
            json!({"is_mcp_tool_call":true,"mcp":{"server":"other","tool":"read_fixture"}}),
            json!({"is_mcp_tool_call":true,"mcp":{"server":"chaos","tool":"read_file"}}),
        ] {
            assert!(!fixture_permission_identity(
                &altered,
                &json!({"arguments":{}})
            ));
        }
    }

    #[test]
    fn native_unknown_and_incomplete_calls_are_denied() {
        for name in [
            "view_file",
            "run_command",
            "write_to_file",
            "read_url",
            "unknown",
        ] {
            assert_eq!(
                decision(&json!({"toolCall":{"name":name,"args":{}}}), true),
                "deny"
            );
        }
        for request in [
            json!(null),
            json!({}),
            json!({"toolCall":{"name":"call_mcp_tool"}}),
        ] {
            assert_eq!(decision(&request, true), "deny");
        }
    }

    #[test]
    fn audit_identifiers_reject_unbounded_or_non_identifier_text() {
        assert_eq!(safe_identifier("view_file"), Some("view_file"));
        for value in ["", "tool secret", "tool\n", "\u{1b}[31m", &"x".repeat(129)] {
            assert!(safe_identifier(value).is_none());
        }
    }
}
