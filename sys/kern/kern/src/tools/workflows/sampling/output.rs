use crate::client_common::{ResponseEvent, ResponseStream};
use chaos_ipc::models::{ContentItem, ResponseItem};
use chaos_ipc::protocol::{RateLimitSnapshot, TokenUsage};
use rama::futures::StreamExt;
use std::io::Write;

const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_EVENTS: usize = 4096;
const MAX_RATE_LIMITS: usize = 64;

pub(super) struct Collected {
    pub(super) text: String,
    pub(super) model: Option<String>,
    pub(super) usage: Option<TokenUsage>,
    pub(super) rate_limits: Vec<RateLimitSnapshot>,
}

/// A serialization byte budget without allocating a second copy of provider
/// payloads, including encrypted reasoning or unexpected tool arguments.
struct Budget(usize);
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| std::io::Error::other("sampling output byte limit exceeded"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Budget {
    fn count(&mut self, value: &impl serde::Serialize) -> Result<(), String> {
        serde_json::to_writer(self, value)
            .map_err(|_| "native Sample output exceeds 256 KiB".into())
    }
}

pub(super) async fn collect(mut stream: ResponseStream) -> Result<Collected, String> {
    let mut budget = Budget(MAX_OUTPUT_BYTES);
    let mut deltas = String::new();
    let mut complete_text = String::new();
    let mut completed_message = false;
    let mut model = None;
    let mut rate_limits = Vec::new();
    for _ in 0..MAX_EVENTS {
        let event = stream
            .next()
            .await
            .ok_or("provider sampling stream ended without completion")?
            .map_err(|error| error.to_string())?;
        match event {
            ResponseEvent::OutputTextDelta(delta) => {
                budget.count(&delta)?;
                deltas.push_str(&delta);
            }
            ResponseEvent::OutputItemAdded(item) => {
                budget.count(&item)?;
                // Reject actions as soon as announced, but do not mistake an
                // empty/in-progress message for a completed text response.
                let _ = item_text(item)?;
            }
            ResponseEvent::OutputItemDone(item) => {
                budget.count(&item)?;
                if let Some(text) = item_text(item)? {
                    complete_text.push_str(&text);
                    completed_message = true;
                }
            }
            ResponseEvent::ReasoningSummaryDelta { delta, .. }
            | ResponseEvent::ReasoningContentDelta { delta, .. } => budget.count(&delta)?,
            ResponseEvent::ServerModel(name) => {
                budget.count(&name)?;
                model = Some(name);
            }
            ResponseEvent::RateLimits(limits) => {
                budget.count(&limits)?;
                if rate_limits.len() >= MAX_RATE_LIMITS {
                    return Err("native Sample rate-limit event limit exceeded".into());
                }
                rate_limits.push(limits);
            }
            ResponseEvent::Completed {
                response_id,
                token_usage,
            } => {
                budget.count(&response_id)?;
                return Ok(Collected {
                    text: if completed_message {
                        complete_text
                    } else {
                        deltas
                    },
                    model,
                    usage: token_usage,
                    rate_limits,
                });
            }
            ResponseEvent::ModelsEtag(etag) => budget.count(&etag)?,
            ResponseEvent::Created
            | ResponseEvent::ServerReasoningIncluded(_)
            | ResponseEvent::ReasoningSummaryPartAdded { .. }
            | ResponseEvent::ModelContextWindow { .. } => {}
        }
    }
    Err("native Sample provider event limit exceeded".into())
}

fn item_text(item: ResponseItem) -> Result<Option<String>, String> {
    match item {
        ResponseItem::Message { role, content, .. } if role == "assistant" => {
            let mut text = String::new();
            for block in content {
                match block {
                    ContentItem::OutputText { text: part } => text.push_str(&part),
                    _ => return Err("native Sample received non-text output".into()),
                }
            }
            Ok(Some(text))
        }
        ResponseItem::Reasoning { .. } => Ok(None),
        _ => Err("native Sample refused a provider tool/action response".into()),
    }
}
