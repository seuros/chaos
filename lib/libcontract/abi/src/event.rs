//! Events emitted by model adapters during a streaming turn.

use chaos_ipc::models::ResponseItem;
use chaos_ipc::protocol::RateLimitSnapshot;
use chaos_ipc::protocol::TokenUsage;

/// A single event from a streaming model turn.
///
/// Adapters translate provider-specific SSE / streaming events into these
/// canonical variants so the core never sees wire-format details.
#[derive(Debug)]
pub enum TurnEvent {
    /// The stream has started and the provider accepted the request.
    Created,

    /// A complete response item is available (fully streamed).
    OutputItemDone(ResponseItem),

    /// A response item has been added but may still be streaming.
    OutputItemAdded(ResponseItem),

    /// The actual model used by the provider (may differ from requested).
    ServerModel(String),

    /// The server already accounted for past reasoning tokens.
    ServerReasoningIncluded(bool),

    /// The turn completed successfully.
    Completed {
        response_id: String,
        token_usage: Option<TokenUsage>,
    },

    /// An incremental text chunk for the current output.
    OutputTextDelta(String),

    /// An incremental reasoning summary chunk.
    ReasoningSummaryDelta { delta: String, summary_index: i64 },

    /// An incremental reasoning content (chain-of-thought) chunk.
    ReasoningContentDelta { delta: String, content_index: i64 },

    /// A new reasoning summary section started.
    ReasoningSummaryPartAdded { summary_index: i64 },

    /// Rate-limit information from the provider.
    RateLimits(RateLimitSnapshot),

    /// Model catalog version tag (provider-specific, but harmless to expose).
    ModelsEtag(String),
}
