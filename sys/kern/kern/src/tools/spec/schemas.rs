use chaos_parrot::sanitize::JsonSchema;
use serde_json::Value as JsonValue;
use serde_json::json;
use std::collections::BTreeMap;

pub(crate) fn unified_exec_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "chunk_id": {
                "type": "string",
                "description": "Chunk identifier included when the response reports one."
            },
            "wall_time_seconds": {
                "type": "number",
                "description": "Elapsed wall time spent waiting for output in seconds."
            },
            "exit_code": {
                "type": "number",
                "description": "Process exit code when the command finished during this call."
            },
            "session_id": {
                "type": "number",
                "description": "Session identifier to pass to write_stdin when the process is still running."
            },
            "original_token_count": {
                "type": "number",
                "description": "Approximate token count before output truncation."
            },
            "output": {
                "type": "string",
                "description": "Command output text, possibly truncated."
            }
        },
        "required": ["wall_time_seconds", "output"],
        "additionalProperties": false
    })
}

pub(crate) fn agent_status_output_schema() -> JsonValue {
    json!({
        "oneOf": [
            {
                "type": "string",
                "enum": ["pending_init", "running", "shutdown", "not_found"]
            },
            {
                "type": "object",
                "properties": {
                    "completed": {
                        "type": ["string", "null"]
                    }
                },
                "required": ["completed"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "errored": {
                        "type": "string"
                    }
                },
                "required": ["errored"],
                "additionalProperties": false
            }
        ]
    })
}

pub(crate) fn spawn_agent_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "agent_id": {
                "type": "string",
                "description": "Thread identifier for the spawned agent."
            },
            "nickname": {
                "type": ["string", "null"],
                "description": "User-facing nickname for the spawned agent when available."
            }
        },
        "required": ["agent_id", "nickname"],
        "additionalProperties": false
    })
}

pub(crate) fn send_input_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "submission_id": {
                "type": "string",
                "description": "Identifier for the queued input submission."
            }
        },
        "required": ["submission_id"],
        "additionalProperties": false
    })
}

pub(crate) fn resume_agent_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "status": agent_status_output_schema()
        },
        "required": ["status"],
        "additionalProperties": false
    })
}

pub(crate) fn wait_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "status": {
                "type": "object",
                "description": "Final statuses keyed by agent id for agents that finished before the timeout.",
                "additionalProperties": agent_status_output_schema()
            },
            "timed_out": {
                "type": "boolean",
                "description": "Whether the wait call returned due to timeout before any agent reached a final status."
            }
        },
        "required": ["status", "timed_out"],
        "additionalProperties": false
    })
}

pub(crate) fn close_agent_output_schema() -> JsonValue {
    json!({
        "type": "object",
        "properties": {
            "status": agent_status_output_schema()
        },
        "required": ["status"],
        "additionalProperties": false
    })
}

pub(crate) fn create_network_permissions_schema() -> JsonSchema {
    JsonSchema::Object {
        properties: BTreeMap::from([(
            "enabled".to_string(),
            JsonSchema::Boolean {
                description: Some("Set to true to request network access.".to_string()),
            },
        )]),
        required: None,
        additional_properties: Some(false.into()),
    }
}

pub(crate) fn create_file_system_permissions_schema() -> JsonSchema {
    JsonSchema::Object {
        properties: BTreeMap::from([
            (
                "read".to_string(),
                JsonSchema::Array {
                    items: Box::new(JsonSchema::String { description: None }),
                    description: Some("Absolute paths to grant read access to.".to_string()),
                },
            ),
            (
                "write".to_string(),
                JsonSchema::Array {
                    items: Box::new(JsonSchema::String { description: None }),
                    description: Some("Absolute paths to grant write access to.".to_string()),
                },
            ),
        ]),
        required: None,
        additional_properties: Some(false.into()),
    }
}

pub(crate) fn create_additional_permissions_schema() -> JsonSchema {
    JsonSchema::Object {
        properties: BTreeMap::from([
            ("network".to_string(), create_network_permissions_schema()),
            (
                "file_system".to_string(),
                create_file_system_permissions_schema(),
            ),
        ]),
        required: None,
        additional_properties: Some(false.into()),
    }
}

pub(crate) fn create_request_permissions_schema() -> JsonSchema {
    JsonSchema::Object {
        properties: BTreeMap::from([
            ("network".to_string(), create_network_permissions_schema()),
            (
                "file_system".to_string(),
                create_file_system_permissions_schema(),
            ),
        ]),
        required: None,
        additional_properties: Some(false.into()),
    }
}

pub(crate) fn create_approval_parameters(
    exec_permission_approvals_enabled: bool,
) -> BTreeMap<String, JsonSchema> {
    let mut properties = BTreeMap::from([
        (
            "sandbox_permissions".to_string(),
            JsonSchema::String {
                description: Some(
                    if exec_permission_approvals_enabled {
                        "Sandbox permissions for the command. Use \"with_additional_permissions\" to request additional sandboxed filesystem or network permissions (preferred), or \"require_escalated\" to request running without sandbox restrictions; defaults to \"use_default\"."
                    } else {
                        "Sandbox permissions for the command. Set to \"require_escalated\" to request running without sandbox restrictions; defaults to \"use_default\"."
                    }
                    .to_string(),
                ),
            },
        ),
        (
            "justification".to_string(),
            JsonSchema::String {
                description: Some(
                    r#"Only set if sandbox_permissions is \"require_escalated\".
                    Request approval from the user to run this command outside the sandbox.
                    Phrased as a simple question that summarizes the purpose of the
                    command as it relates to the task at hand - e.g. 'Do you want to
                    fetch and pull the latest version of this git branch?'"#
                    .to_string(),
                ),
            },
        ),
        (
            "prefix_rule".to_string(),
            JsonSchema::Array {
                items: Box::new(JsonSchema::String { description: None }),
                description: Some(
                    r#"Only specify when sandbox_permissions is `require_escalated`.
                        Suggest a prefix command pattern that will allow you to fulfill similar requests from the user in the future.
                        Should be a short but reasonable prefix, e.g. [\"git\", \"pull\"] or [\"uv\", \"run\"] or [\"pytest\"]."#.to_string(),
                ),
            },
        )
    ]);

    if exec_permission_approvals_enabled {
        properties.insert(
            "additional_permissions".to_string(),
            create_additional_permissions_schema(),
        );
    }

    properties
}
