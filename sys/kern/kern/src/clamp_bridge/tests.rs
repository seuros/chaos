use super::*;
use crate::client_common::Prompt;
use crate::client_common::tools::FreeformToolFormat;
use chaos_parrot::sanitize::JsonSchema;
use serde_json::json;

#[test]
fn antigravity_catalogue_projects_current_mcp_tools_not_native_tools() {
    let parameters = JsonSchema::Object {
        properties: Default::default(),
        required: None,
        additional_properties: None,
    };
    let mut prompt = Prompt::default();
    prompt.base_instructions.text = "Canonical instructions".into();
    prompt.tools = vec![
        ToolSpec::Function(ResponsesApiTool {
            name: "lookup".into(),
            description: "Read data".into(),
            strict: false,
            defer_loading: None,
            parameters: parameters.clone(),
            output_schema: Some(json!({"type": "string"})),
        }),
        ToolSpec::Freeform(FreeformTool {
            name: "patch".into(),
            description: "Apply changes".into(),
            format: FreeformToolFormat {
                r#type: "grammar".into(),
                syntax: "lark".into(),
                definition: "unused".into(),
            },
        }),
        ToolSpec::LocalShell {},
        ToolSpec::ImageGeneration {
            output_format: "png".into(),
        },
        ToolSpec::WebSearch {
            external_web_access: None,
            filters: None,
            user_location: None,
            search_context_size: None,
            search_content_types: None,
        },
        ToolSpec::ToolSearch {
            execution: "server".into(),
            description: "Native search".into(),
            parameters: parameters.clone(),
        },
    ];
    let rendered = antigravity_system_prompt(&prompt);
    assert!(rendered.starts_with("Canonical instructions\n\n"));
    assert!(rendered.contains("ServerName \"chaos\""));
    let tools: serde_json::Value =
        serde_json::from_str(rendered.rsplit_once("\n\n").unwrap().1).unwrap();
    assert_eq!(tools.as_array().unwrap().len(), 2);
    assert_eq!(tools[0]["name"], "lookup");
    assert_eq!(
        tools[0]["inputSchema"],
        serde_json::Value::from(&parameters)
    );
    assert_eq!(tools[0]["outputSchema"], json!({"type": "string"}));
    assert_eq!(tools[1]["name"], "patch");
    assert_eq!(
        tools[1]["inputSchema"],
        json!({
            "type": "object", "properties": {"input": {
                "type": "string", "description": "Freeform grammar input (syntax: lark)."
            }}, "required": ["input"], "additionalProperties": false
        })
    );
    // The next sampling turn must not advertise a removed tool.
    prompt.tools.remove(0);
    let next = antigravity_system_prompt(&prompt);
    assert!(!next.contains("lookup"));
    assert!(next.contains("patch"));
    prompt.tools.clear();
    prompt.base_instructions.text = "Changed instructions".into();
    let empty = antigravity_system_prompt(&prompt);
    assert!(empty.starts_with("Changed instructions\n\n"));
    assert!(empty.ends_with("[]"));
}
