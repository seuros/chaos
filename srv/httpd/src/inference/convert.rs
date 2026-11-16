//! `lsd-protocol` requests into kernel inputs, kernel events into protocol
//! chunks. The kernel never sees the wire contract.

use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::models::ContentItem;
use chaos_ipc::models::FunctionCallOutputBody;
use chaos_ipc::models::FunctionCallOutputPayload;
use chaos_ipc::models::LocalShellAction;
use chaos_ipc::models::ReasoningItemContent;
use chaos_ipc::models::ReasoningItemReasoningSummary;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::TokenUsage;
use chaos_kern::ResponseEvent;
use lsd_protocol::Content;
use lsd_protocol::ContentBlockChunk;
use lsd_protocol::Effort;
use lsd_protocol::File;
use lsd_protocol::FinishReason;
use lsd_protocol::InferenceRequest;
use lsd_protocol::Message;
use lsd_protocol::OutputContent;
use lsd_protocol::ResponseToolCall;
use lsd_protocol::Role;
use lsd_protocol::System;
use lsd_protocol::ThinkingDisplay;
use lsd_protocol::Thought;
use lsd_protocol::Tool;
use lsd_protocol::ToolChoice;
use lsd_protocol::Usage;
use lsd_protocol::content::ThoughtSummary;

/// What the kernel needs from a request, once the request is known to be
/// one chaos can serve.
pub(super) struct Turn {
    pub(super) instructions: String,
    pub(super) input: Vec<ResponseItem>,
    pub(super) tools: Vec<DynamicToolSpec>,
    pub(super) parallel_tool_calls: bool,
    pub(super) effort: Option<ReasoningEffort>,
    pub(super) summary: Option<ReasoningSummary>,
}

pub(super) fn turn(request: &InferenceRequest) -> Result<Turn, String> {
    if request.variant_name.is_some() {
        return Err("chaos serve has no variants".into());
    }
    let instructions = match &request.input.system {
        None => String::new(),
        Some(System::Text(text)) => text.clone(),
        Some(System::Arguments(_)) => {
            return Err(
                "chaos serve has no system templates; send the system prompt as text".into(),
            );
        }
    };
    let mut input = Vec::new();
    for message in &request.input.messages {
        message_items(message, &mut input)?;
    }
    for message in &request.input.ephemeral {
        message_items(message, &mut input)?;
    }
    let tools = match request.tool_choice {
        None | Some(ToolChoice::Auto) => tools(request),
        Some(ToolChoice::None) => Vec::new(),
        Some(ToolChoice::Required | ToolChoice::Specific(_)) => {
            return Err("chaos serve does not force tool calls; use tool_choice auto".into());
        }
    };
    let params = &request.params.chat_completion;
    Ok(Turn {
        instructions,
        input,
        parallel_tool_calls: !tools.is_empty() && request.parallel_tool_calls.unwrap_or(true),
        tools,
        effort: params.reasoning_effort.map(effort),
        summary: params.thinking_display.map(|display| match display {
            ThinkingDisplay::Omitted => ReasoningSummary::None,
            ThinkingDisplay::Summarized => ReasoningSummary::Auto,
        }),
    })
}

fn tools(request: &InferenceRequest) -> Vec<DynamicToolSpec> {
    request
        .additional_tools
        .iter()
        .flatten()
        .filter(|tool| {
            request
                .allowed_tools
                .as_ref()
                .is_none_or(|allowed| allowed.iter().any(|name| name == tool.name()))
        })
        .map(|tool| match tool {
            Tool::Function {
                name,
                description,
                parameters,
                ..
            } => DynamicToolSpec {
                name: name.clone(),
                description: description.clone(),
                input_schema: parameters.clone(),
                defer_loading: false,
            },
        })
        .collect()
}

fn effort(effort: Effort) -> ReasoningEffort {
    match effort {
        Effort::None => ReasoningEffort::None,
        Effort::Minimal => ReasoningEffort::Minimal,
        Effort::Low => ReasoningEffort::Low,
        Effort::Medium => ReasoningEffort::Medium,
        Effort::High => ReasoningEffort::High,
        Effort::Xhigh => ReasoningEffort::XHigh,
        Effort::Max => ReasoningEffort::Max,
    }
}

fn message_items(message: &Message, items: &mut Vec<ResponseItem>) -> Result<(), String> {
    let mut text: Vec<ContentItem> = Vec::new();
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "developer",
    };
    let flush = |text: &mut Vec<ContentItem>, items: &mut Vec<ResponseItem>| {
        if !text.is_empty() {
            items.push(ResponseItem::Message {
                id: None,
                role: role.to_string(),
                content: std::mem::take(text),
                end_turn: None,
                phase: None,
            });
        }
    };
    for content in &message.content {
        match (message.role, content) {
            (Role::Assistant, Content::Text { text: body }) => {
                text.push(ContentItem::OutputText { text: body.clone() });
            }
            (_, Content::Text { text: body } | Content::RawText { value: body }) => {
                text.push(ContentItem::InputText { text: body.clone() });
            }
            (Role::User, Content::File(file)) => text.push(ContentItem::InputImage {
                image_url: file_url(file),
            }),
            (Role::User, Content::ToolResult(result)) => {
                flush(&mut text, items);
                items.push(ResponseItem::FunctionCallOutput {
                    call_id: result.id.clone(),
                    output: FunctionCallOutputPayload {
                        body: FunctionCallOutputBody::Text(result.result.clone()),
                        success: Some(!result.is_error),
                    },
                    tool_name: Some(result.name.clone()),
                });
            }
            (Role::Assistant, Content::Thought(thought)) => {
                flush(&mut text, items);
                items.push(reasoning(thought));
            }
            (Role::Assistant, Content::ToolCall(call)) => {
                flush(&mut text, items);
                items.push(ResponseItem::FunctionCall {
                    id: None,
                    name: call.name.clone(),
                    namespace: None,
                    arguments: call.arguments.clone(),
                    call_id: call.id.clone(),
                    provider_metadata: None,
                });
            }
            (_, Content::Unknown(_)) => {}
            (role, other) => {
                return Err(format!(
                    "a {} block cannot appear in a {role:?} message",
                    block_name(other)
                ));
            }
        }
    }
    flush(&mut text, items);
    Ok(())
}

fn block_name(content: &Content) -> &'static str {
    match content {
        Content::Text { .. } => "text",
        Content::ToolCall(_) => "tool_call",
        Content::ToolResult(_) => "tool_result",
        Content::RawText { .. } => "raw_text",
        Content::Thought(_) => "thought",
        Content::File(_) => "file",
        Content::Unknown(_) => "unknown",
    }
}

fn file_url(file: &File) -> String {
    match file {
        File::Url { url, .. } => url.clone(),
        File::Base64 {
            mime_type, data, ..
        } => format!("data:{mime_type};base64,{data}"),
    }
}

fn reasoning(thought: &Thought) -> ResponseItem {
    ResponseItem::Reasoning {
        id: String::new(),
        summary: thought
            .summary
            .iter()
            .flatten()
            .map(|ThoughtSummary::SummaryText { text }| {
                ReasoningItemReasoningSummary::SummaryText { text: text.clone() }
            })
            .collect(),
        content: thought
            .text
            .as_ref()
            .filter(|text| !text.is_empty())
            .map(|text| vec![ReasoningItemContent::ReasoningText { text: text.clone() }]),
        encrypted_content: thought.signature.clone(),
    }
}

/// Folds kernel events into protocol content: deltas as they come for the
/// stream, and the finished blocks for a non-streaming body.
#[derive(Default)]
pub(super) struct Projection {
    streamed_text: bool,
    tool_called: bool,
    pub(super) output: Vec<OutputContent>,
}

pub(super) enum Step {
    Content(Vec<ContentBlockChunk>),
    Finished {
        usage: Option<Usage>,
        finish_reason: FinishReason,
    },
    Nothing,
}

const TEXT_BLOCK: &str = "text";
const THOUGHT_BLOCK: &str = "thought";

impl Projection {
    pub(super) fn step(&mut self, event: ResponseEvent) -> Step {
        match event {
            ResponseEvent::OutputTextDelta(text) => {
                self.streamed_text = true;
                Step::Content(vec![ContentBlockChunk::Text {
                    id: TEXT_BLOCK.into(),
                    text,
                }])
            }
            ResponseEvent::ReasoningContentDelta { delta, .. } => {
                Step::Content(vec![thought_chunk(Some(delta), None, None)])
            }
            ResponseEvent::ReasoningSummaryDelta { delta, .. } => {
                Step::Content(vec![thought_chunk(None, None, Some(delta))])
            }
            ResponseEvent::OutputItemDone(item) => self.done(item),
            ResponseEvent::Completed { token_usage, .. } => Step::Finished {
                usage: token_usage.as_ref().map(usage),
                finish_reason: if self.tool_called {
                    FinishReason::ToolCall
                } else {
                    FinishReason::Stop
                },
            },
            _ => Step::Nothing,
        }
    }

    fn done(&mut self, item: ResponseItem) -> Step {
        match item {
            ResponseItem::Message { role, content, .. } if role == "assistant" => {
                let text: String = content
                    .iter()
                    .filter_map(|part| match part {
                        ContentItem::OutputText { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let streamed = std::mem::take(&mut self.streamed_text);
                if text.is_empty() {
                    return Step::Nothing;
                }
                self.output.push(OutputContent::Text { text: text.clone() });
                if streamed {
                    Step::Nothing
                } else {
                    Step::Content(vec![ContentBlockChunk::Text {
                        id: TEXT_BLOCK.into(),
                        text,
                    }])
                }
            }
            ResponseItem::Reasoning {
                summary,
                content,
                encrypted_content,
                ..
            } => {
                let text: String = content
                    .iter()
                    .flatten()
                    .map(|part| match part {
                        ReasoningItemContent::ReasoningText { text }
                        | ReasoningItemContent::Text { text } => text.as_str(),
                    })
                    .collect();
                let summary: Vec<ThoughtSummary> = summary
                    .into_iter()
                    .map(|ReasoningItemReasoningSummary::SummaryText { text }| {
                        ThoughtSummary::SummaryText { text }
                    })
                    .collect();
                self.output.push(OutputContent::Thought(Thought {
                    text: Some(text),
                    signature: encrypted_content.clone(),
                    summary: (!summary.is_empty()).then_some(summary),
                    ..Default::default()
                }));
                match encrypted_content {
                    Some(signature) => {
                        Step::Content(vec![thought_chunk(None, Some(signature), None)])
                    }
                    None => Step::Nothing,
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => self.tool_call(call_id, name, arguments),
            ResponseItem::CustomToolCall {
                call_id,
                name,
                input,
                ..
            } => self.tool_call(
                call_id,
                name,
                serde_json::json!({ "input": input }).to_string(),
            ),
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                action: LocalShellAction::Exec(exec),
                ..
            } => self.tool_call(
                call_id,
                "shell".into(),
                serde_json::json!({ "command": exec.command.join(" ") }).to_string(),
            ),
            _ => Step::Nothing,
        }
    }

    fn tool_call(&mut self, id: String, name: String, arguments: String) -> Step {
        self.tool_called = true;
        self.output.push(OutputContent::ToolCall(ResponseToolCall {
            id: id.clone(),
            raw_name: name.clone(),
            arguments: serde_json::from_str(&arguments).ok(),
            name: Some(name.clone()),
            raw_arguments: arguments.clone(),
        }));
        Step::Content(vec![ContentBlockChunk::ToolCall {
            id,
            raw_name: Some(name),
            raw_arguments: arguments,
        }])
    }
}

fn thought_chunk(
    text: Option<String>,
    signature: Option<String>,
    summary: Option<String>,
) -> ContentBlockChunk {
    ContentBlockChunk::Thought {
        id: THOUGHT_BLOCK.into(),
        summary_id: summary.as_ref().map(|_| "0".to_string()),
        text,
        signature,
        summary_text: summary,
        provider_type: None,
        extra_data: None,
    }
}

fn usage(usage: &TokenUsage) -> Usage {
    let count = |value: i64| u32::try_from(value.max(0)).ok();
    Usage {
        input_tokens: count(usage.input_tokens),
        output_tokens: count(usage.output_tokens),
        provider_cache_read_input_tokens: count(usage.cache_read_input_tokens).filter(|n| *n > 0),
        provider_cache_write_input_tokens: count(usage.cache_creation_input_tokens)
            .filter(|n| *n > 0),
        cost: None,
    }
}
