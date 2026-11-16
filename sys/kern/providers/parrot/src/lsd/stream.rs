use chaos_abi::AbiError;
use chaos_abi::ContentItem;
use chaos_abi::ResponseItem;
use chaos_abi::TokenUsage;
use chaos_abi::TurnEvent;
use chaos_ipc::models::ReasoningItemContent;
use chaos_ipc::models::ReasoningItemReasoningSummary;
use lsd_protocol::ContentBlockChunk;
use lsd_protocol::FinishReason;
use lsd_protocol::InferenceResponseChunk;
use lsd_protocol::StopDetails;
use lsd_protocol::Usage;
use uuid::Uuid;

#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct Reasoning {
    text: String,
    summary: String,
    signature: String,
}

/// Folds a trip's chunk stream into turn events. Deltas sharing a block id
/// concatenate; everything finalizes when the chunk carrying
/// `finish_reason` arrives.
#[derive(Default)]
pub(super) struct ChunkFolder {
    started: bool,
    text: Option<String>,
    reasoning: Option<Reasoning>,
    tool_calls: Vec<ToolCall>,
}

pub(super) struct Finished {
    pub(super) trip_id: Uuid,
}

impl ChunkFolder {
    pub(super) fn fold(
        &mut self,
        chunk: InferenceResponseChunk,
        response_id: &mut String,
    ) -> Result<(Vec<TurnEvent>, Option<Finished>), AbiError> {
        let mut events = Vec::new();
        if !self.started {
            self.started = true;
            *response_id = chunk.inference_id().to_string();
            events.push(TurnEvent::Created);
        }
        let trip_id = chunk.trip_id();

        let (content, usage, finish_reason, stop_details) = match chunk {
            InferenceResponseChunk::Chat {
                content,
                usage,
                finish_reason,
                stop_details,
                ..
            } => (content, usage, finish_reason, stop_details),
            InferenceResponseChunk::Json {
                raw,
                usage,
                finish_reason,
                stop_details,
                ..
            } => (
                vec![ContentBlockChunk::Text {
                    id: String::new(),
                    text: raw,
                }],
                usage,
                finish_reason,
                stop_details,
            ),
        };

        for block in content {
            self.block(block, &mut events);
        }

        let Some(finish_reason) = finish_reason else {
            return Ok((events, None));
        };
        if finish_reason == FinishReason::Refusal {
            return Err(refusal(stop_details));
        }
        self.finish(&mut events);
        events.push(TurnEvent::Completed {
            response_id: response_id.clone(),
            token_usage: usage.and_then(token_usage),
        });
        Ok((events, Some(Finished { trip_id })))
    }

    fn block(&mut self, block: ContentBlockChunk, events: &mut Vec<TurnEvent>) {
        match block {
            ContentBlockChunk::Text { text, .. } if !text.is_empty() => {
                if self.text.is_none() {
                    events.push(TurnEvent::OutputItemAdded(ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![],
                        phase: None,
                        end_turn: None,
                    }));
                }
                self.text.get_or_insert_default().push_str(&text);
                events.push(TurnEvent::OutputTextDelta(text));
            }
            ContentBlockChunk::Thought {
                text,
                signature,
                summary_text,
                ..
            } => {
                if self.reasoning.is_none() {
                    events.push(TurnEvent::OutputItemAdded(ResponseItem::Reasoning {
                        id: String::new(),
                        summary: vec![],
                        content: None,
                        encrypted_content: None,
                    }));
                }
                let reasoning = self.reasoning.get_or_insert_default();
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    reasoning.text.push_str(&text);
                    events.push(TurnEvent::ReasoningContentDelta {
                        delta: text,
                        content_index: 0,
                    });
                }
                if let Some(summary) = summary_text.filter(|text| !text.is_empty()) {
                    reasoning.summary.push_str(&summary);
                    events.push(TurnEvent::ReasoningSummaryDelta {
                        delta: summary,
                        summary_index: 0,
                    });
                }
                if let Some(signature) = signature {
                    reasoning.signature.push_str(&signature);
                }
            }
            ContentBlockChunk::ToolCall {
                id,
                raw_name,
                raw_arguments,
            } => {
                let index = match self.tool_calls.iter().position(|call| call.id == id) {
                    Some(index) => index,
                    None => {
                        self.tool_calls.push(ToolCall {
                            id,
                            ..Default::default()
                        });
                        self.tool_calls.len() - 1
                    }
                };
                let call = &mut self.tool_calls[index];
                if let Some(name) = raw_name.filter(|name| !name.is_empty()) {
                    call.name = name;
                }
                call.arguments.push_str(&raw_arguments);
            }
            ContentBlockChunk::Text { .. } | ContentBlockChunk::Unknown { .. } => {}
        }
    }

    fn finish(&mut self, events: &mut Vec<TurnEvent>) {
        if let Some(reasoning) = self.reasoning.take() {
            events.push(TurnEvent::OutputItemDone(ResponseItem::Reasoning {
                id: String::new(),
                summary: (!reasoning.summary.is_empty())
                    .then_some(ReasoningItemReasoningSummary::SummaryText {
                        text: reasoning.summary,
                    })
                    .into_iter()
                    .collect(),
                content: (!reasoning.text.is_empty()).then(|| {
                    vec![ReasoningItemContent::ReasoningText {
                        text: reasoning.text,
                    }]
                }),
                encrypted_content: (!reasoning.signature.is_empty()).then_some(reasoning.signature),
            }));
        }
        if let Some(text) = self.text.take() {
            events.push(TurnEvent::OutputItemDone(ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText { text }],
                phase: None,
                end_turn: None,
            }));
        }
        for call in self.tool_calls.drain(..) {
            events.push(TurnEvent::OutputItemDone(ResponseItem::FunctionCall {
                id: None,
                name: call.name,
                arguments: if call.arguments.is_empty() {
                    "{}".to_string()
                } else {
                    call.arguments
                },
                call_id: call.id,
                namespace: None,
                provider_metadata: None,
            }));
        }
    }
}

fn refusal(details: Option<StopDetails>) -> AbiError {
    let details = details.unwrap_or_default();
    AbiError::Refusal {
        category: details.category,
        explanation: details.explanation,
    }
}

fn token_usage(usage: Usage) -> Option<TokenUsage> {
    let input_tokens = i64::from(usage.input_tokens?);
    let output_tokens = i64::from(usage.output_tokens?);
    Some(TokenUsage {
        input_tokens,
        output_tokens,
        total_tokens: input_tokens + output_tokens,
        cache_read_input_tokens: usage.provider_cache_read_input_tokens.map_or(0, i64::from),
        cache_creation_input_tokens: usage.provider_cache_write_input_tokens.map_or(0, i64::from),
        ..Default::default()
    })
}
