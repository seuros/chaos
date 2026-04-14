//! OpenAI adapter — implements [`ModelAdapter`] for the OpenAI Responses API.
//!
//! This module bridges between the provider-neutral Chaos-ABI types and
//! the OpenAI-specific wire format used by [`ResponsesClient`].

use crate::common::OpenAiVerbosity;
use crate::common::Reasoning;
use crate::common::ResponseEvent;
use crate::common::ResponsesApiRequest;
use crate::common::TextControls;
use crate::common::TextFormat;
use crate::common::TextFormatType;
use crate::representer::Representer;
use chaos_abi::AbiError;
use chaos_abi::ToolDef;
use chaos_abi::TurnEvent;
use chaos_abi::TurnRequest;
use chaos_ipc::config_types::Verbosity as VerbosityConfig;
use serde_json::Value;

// ---------------------------------------------------------------------------
// TurnRequest → ResponsesApiRequest
// ---------------------------------------------------------------------------

/// Convert a [`TurnRequest`] (Chaos-ABI) into a [`ResponsesApiRequest`] (wire format).
///
/// The `representer` controls how input items are projected:
/// - [`crate::representer::ResponsesRepresenter`] for real OpenAI (remaps `system→developer`)
/// - [`crate::representer::OpenwAInnabeRepresenter`] for xAI and compat clones (`system` unchanged,
///   `Reasoning` items dropped)
///
/// Callers obtain the correct representer from the session's [`crate::representer::SessionRepresenter`].
pub(crate) fn turn_request_to_api_request(
    req: TurnRequest,
    representer: &dyn Representer,
) -> ResponsesApiRequest {
    let tools: Vec<Value> = req
        .extensions
        .get("openai_tools")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| req.tools.into_iter().map(tool_def_to_openai).collect());

    let reasoning = req.reasoning.map(|r| Reasoning {
        effort: r.effort,
        summary: r.summary,
    });

    let include = if reasoning.is_some() {
        vec!["reasoning.encrypted_content".to_string()]
    } else {
        Vec::new()
    };

    let text = build_text_controls(req.verbosity, &req.output_schema);

    // Read provider-specific fields from extensions.
    let store = req
        .extensions
        .get("store")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let service_tier = req
        .extensions
        .get("service_tier")
        .and_then(Value::as_str)
        .map(String::from);
    let prompt_cache_key = req
        .extensions
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .map(String::from);

    ResponsesApiRequest {
        model: req.model,
        instructions: req.instructions,
        input: representer.represent(req.input),
        tools,
        tool_choice: "auto".to_string(),
        parallel_tool_calls: req.parallel_tool_calls,
        reasoning,
        store,
        stream: true,
        include,
        service_tier,
        prompt_cache_key,
        text,
    }
}

// ---------------------------------------------------------------------------
// ToolDef → OpenAI JSON
// ---------------------------------------------------------------------------

fn tool_def_to_openai(tool: ToolDef) -> Value {
    match tool {
        ToolDef::Function(f) => {
            serde_json::json!({
                "type": "function",
                "name": f.name,
                "description": f.description,
                "strict": f.strict,
                "parameters": f.parameters,
            })
        }
        ToolDef::Freeform(f) => {
            serde_json::json!({
                "type": "custom",
                "name": f.name,
                "description": f.description,
                "format": {
                    "type": f.format_type,
                    "syntax": f.syntax,
                    "definition": f.definition,
                }
            })
        }
    }
}

// ---------------------------------------------------------------------------
// TextControls builder
// ---------------------------------------------------------------------------

fn build_text_controls(
    verbosity: Option<VerbosityConfig>,
    output_schema: &Option<Value>,
) -> Option<TextControls> {
    if verbosity.is_none() && output_schema.is_none() {
        return None;
    }

    Some(TextControls {
        verbosity: verbosity.map(OpenAiVerbosity::from),
        format: output_schema.as_ref().map(|schema| TextFormat {
            r#type: TextFormatType::JsonSchema,
            strict: true,
            schema: schema.clone(),
            name: "codex_output_schema".to_string(),
        }),
    })
}

// ---------------------------------------------------------------------------
// ResponseEvent ↔ TurnEvent conversions
// ---------------------------------------------------------------------------

impl From<ResponseEvent> for TurnEvent {
    fn from(event: ResponseEvent) -> Self {
        match event {
            ResponseEvent::Created => TurnEvent::Created,
            ResponseEvent::OutputItemDone(item) => TurnEvent::OutputItemDone(item),
            ResponseEvent::OutputItemAdded(item) => TurnEvent::OutputItemAdded(item),
            ResponseEvent::ServerModel(model) => TurnEvent::ServerModel(model),
            ResponseEvent::ServerReasoningIncluded(v) => TurnEvent::ServerReasoningIncluded(v),
            ResponseEvent::Completed {
                response_id,
                token_usage,
            } => TurnEvent::Completed {
                response_id,
                token_usage,
            },
            ResponseEvent::OutputTextDelta(delta) => TurnEvent::OutputTextDelta(delta),
            ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => TurnEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            },
            ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => TurnEvent::ReasoningContentDelta {
                delta,
                content_index,
            },
            ResponseEvent::ReasoningSummaryPartAdded { summary_index } => {
                TurnEvent::ReasoningSummaryPartAdded { summary_index }
            }
            ResponseEvent::RateLimits(snapshot) => TurnEvent::RateLimits(snapshot),
            ResponseEvent::ModelsEtag(etag) => TurnEvent::ModelsEtag(etag),
        }
    }
}

impl From<TurnEvent> for ResponseEvent {
    fn from(event: TurnEvent) -> Self {
        match event {
            TurnEvent::Created => ResponseEvent::Created,
            TurnEvent::OutputItemDone(item) => ResponseEvent::OutputItemDone(item),
            TurnEvent::OutputItemAdded(item) => ResponseEvent::OutputItemAdded(item),
            TurnEvent::ServerModel(model) => ResponseEvent::ServerModel(model),
            TurnEvent::ServerReasoningIncluded(v) => ResponseEvent::ServerReasoningIncluded(v),
            TurnEvent::Completed {
                response_id,
                token_usage,
            } => ResponseEvent::Completed {
                response_id,
                token_usage,
            },
            TurnEvent::OutputTextDelta(delta) => ResponseEvent::OutputTextDelta(delta),
            TurnEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            } => ResponseEvent::ReasoningSummaryDelta {
                delta,
                summary_index,
            },
            TurnEvent::ReasoningContentDelta {
                delta,
                content_index,
            } => ResponseEvent::ReasoningContentDelta {
                delta,
                content_index,
            },
            TurnEvent::ReasoningSummaryPartAdded { summary_index } => {
                ResponseEvent::ReasoningSummaryPartAdded { summary_index }
            }
            TurnEvent::RateLimits(snapshot) => ResponseEvent::RateLimits(snapshot),
            TurnEvent::ModelsEtag(etag) => ResponseEvent::ModelsEtag(etag),
        }
    }
}

// ---------------------------------------------------------------------------
// ApiError → AbiError
// ---------------------------------------------------------------------------

impl From<crate::error::ApiError> for AbiError {
    fn from(err: crate::error::ApiError) -> Self {
        match err {
            crate::error::ApiError::ContextWindowExceeded => AbiError::ContextWindowExceeded,
            crate::error::ApiError::QuotaExceeded => AbiError::QuotaExceeded,
            crate::error::ApiError::UsageNotIncluded => AbiError::UsageNotIncluded,
            crate::error::ApiError::ServerOverloaded => AbiError::ServerOverloaded,
            crate::error::ApiError::InvalidRequest { message } => {
                AbiError::InvalidRequest { message }
            }
            crate::error::ApiError::Stream(msg) => AbiError::Stream(msg),
            crate::error::ApiError::Transport(t) => match t {
                crate::TransportError::Http { status, body, .. } => AbiError::Transport {
                    status: status.as_u16(),
                    message: body.unwrap_or_else(|| status.to_string()),
                },
                other => AbiError::Transport {
                    status: 0,
                    message: other.to_string(),
                },
            },
            crate::error::ApiError::Api { status, message } => AbiError::Transport {
                status: status.as_u16(),
                message,
            },
            crate::error::ApiError::RateLimit(msg) => AbiError::Retryable {
                message: msg,
                delay: None,
            },
            crate::error::ApiError::Retryable { message, delay } => {
                AbiError::Retryable { message, delay }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::representer::ResponsesRepresenter;
    use chaos_abi::FunctionToolDef;
    use chaos_abi::ReasoningConfig;
    use serde_json::json;

    fn make_req(model: &str) -> TurnRequest {
        TurnRequest {
            model: model.to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        }
    }

    #[test]
    fn turn_request_converts_to_responses_api_request() {
        let req = TurnRequest {
            model: "gpt-4o".to_string(),
            instructions: "Be helpful.".to_string(),
            input: vec![],
            tools: vec![ToolDef::Function(FunctionToolDef {
                name: "get_weather".to_string(),
                description: "Get weather for a location".to_string(),
                parameters: json!({"type": "object", "properties": {"location": {"type": "string"}}}),
                strict: true,
            })],
            parallel_tool_calls: true,
            reasoning: Some(ReasoningConfig {
                effort: Some(chaos_abi::ReasoningEffort::High),
                summary: None,
            }),
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions: serde_json::Map::new(),
        };

        let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);

        assert_eq!(api_req.model, "gpt-4o");
        assert_eq!(api_req.instructions, "Be helpful.");
        assert_eq!(api_req.tool_choice, "auto");
        assert!(api_req.parallel_tool_calls);
        assert!(api_req.stream);
        assert!(!api_req.store);
        assert_eq!(api_req.tools.len(), 1);
        assert_eq!(api_req.tools[0]["name"], "get_weather");
        assert!(api_req.reasoning.is_some());
        assert_eq!(
            api_req.include,
            vec!["reasoning.encrypted_content".to_string()]
        );
    }

    #[test]
    fn extensions_populate_openai_specific_fields() {
        let mut extensions = serde_json::Map::new();
        extensions.insert("store".to_string(), json!(true));
        extensions.insert("service_tier".to_string(), json!("priority"));
        extensions.insert("prompt_cache_key".to_string(), json!("conv-123"));

        let req = TurnRequest {
            model: "gpt-4o".to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions,
        };

        let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);

        assert!(api_req.store);
        assert_eq!(api_req.service_tier.as_deref(), Some("priority"));
        assert_eq!(api_req.prompt_cache_key.as_deref(), Some("conv-123"));
    }

    #[test]
    fn openai_tools_extension_overrides_neutral_tool_conversion() {
        let mut extensions = serde_json::Map::new();
        extensions.insert(
            "openai_tools".to_string(),
            json!([{
                "type": "local_shell"
            }]),
        );

        let req = TurnRequest {
            model: "gpt-4o".to_string(),
            instructions: String::new(),
            input: vec![],
            tools: vec![ToolDef::Function(FunctionToolDef {
                name: "should_not_be_used".to_string(),
                description: "ignored".to_string(),
                parameters: json!({"type": "object"}),
                strict: false,
            })],
            parallel_tool_calls: false,
            reasoning: None,
            output_schema: None,
            verbosity: None,
            turn_state: None,
            extensions,
        };

        let api_req = turn_request_to_api_request(req, &ResponsesRepresenter);
        assert_eq!(api_req.tools, vec![json!({"type": "local_shell"})]);
    }

    #[test]
    fn wannabe_representer_used_via_turn_request_conversion() {
        use crate::representer::OpenwAInnabeRepresenter;
        use chaos_ipc::models::ResponseItem;
        let req = TurnRequest {
            input: vec![
                ResponseItem::Message {
                    id: None,
                    role: "system".into(),
                    content: vec![],
                    end_turn: None,
                    phase: None,
                },
                ResponseItem::Reasoning {
                    id: "rs".into(),
                    summary: vec![],
                    content: None,
                    encrypted_content: None,
                },
            ],
            ..make_req("grok-4")
        };
        let api_req = turn_request_to_api_request(req, &OpenwAInnabeRepresenter);
        assert_eq!(api_req.input.len(), 1, "Reasoning item must be dropped");
        assert!(
            matches!(&api_req.input[0], ResponseItem::Message { role, .. } if role == "system"),
            "system role must pass through unchanged for wannabe"
        );
    }

    #[test]
    fn response_event_roundtrips_through_turn_event() {
        let event = ResponseEvent::OutputTextDelta("hello".to_string());
        let turn: TurnEvent = event.into();
        let back: ResponseEvent = turn.into();
        assert!(matches!(back, ResponseEvent::OutputTextDelta(ref s) if s == "hello"));
    }

    #[test]
    fn http_transport_status_is_preserved_in_abi_error() {
        let err = crate::error::ApiError::Transport(crate::TransportError::Http {
            status: http::StatusCode::UNAUTHORIZED,
            url: Some("https://api.openai.com/v1/responses".to_string()),
            headers: None,
            body: Some("unauthorized".to_string()),
        });

        let abi: AbiError = err.into();
        assert!(matches!(
            abi,
            AbiError::Transport { status: 401, message } if message == "unauthorized"
        ));
    }

    #[test]
    fn freeform_tool_converts_to_custom_json() {
        let tool = ToolDef::Freeform(chaos_abi::FreeformToolDef {
            name: "apply_patch".to_string(),
            description: "Apply a patch".to_string(),
            format_type: "xml".to_string(),
            syntax: "xml-patch".to_string(),
            definition: "<patch>...</patch>".to_string(),
        });

        let json = tool_def_to_openai(tool);
        assert_eq!(json["type"], "custom");
        assert_eq!(json["name"], "apply_patch");
        assert_eq!(json["format"]["type"], "xml");
    }
}
