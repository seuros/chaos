//! Chaos-owned MCP wire contract.
//!
//! Any configured MCP server may speak these methods. The MCP entry name is
//! not part of the protocol.

use std::collections::HashMap;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

/// Inbox wake notification method.
pub const FLEET_INBOX_NOTIFICATION: &str = "notifications/chaos/fleet/inbox";

/// Client-only host information request.
pub const FLEET_HOST_INFO_REQUEST: &str = "chaos/fleet/hostInfo";

/// Inbox resource URI that must accompany a wake hint.
pub const FLEET_INBOX_URI: &str = "fleet://inbox";

/// Initialize `experimental` capability advertised by Chaos clients.
pub const FLEET_EXPERIMENTAL_CAPABILITY: &str = "chaos/fleet";

/// Reserved `_meta` key for host-attested review provenance.
///
/// Ordinary MCP tool calls may not set this key.
pub const REVIEW_PROVENANCE_META_KEY: &str = "chaos/reviewProvenance";

/// Opaque account subject prefix.
pub const ACCOUNT_SUBJECT_PREFIX: &str = "credential:v1:";

/// Opaque model-family subject prefix.
pub const MODEL_FAMILY_SUBJECT_PREFIX: &str = "review-subject:v1:";

/// Opaque review-run subject prefix.
pub const REVIEW_RUN_SUBJECT_PREFIX: &str = "review-run:v1:";

/// Opaque reviewer-attempt subject prefix.
pub const REVIEWER_ATTEMPT_SUBJECT_PREFIX: &str = "reviewer-attempt:v1:";

const MAX_FLEET_INBOX_MESSAGE_IDS: usize = 50;

/// Wake hint params for [`FLEET_INBOX_NOTIFICATION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetInboxHint {
    pub uri: String,
    pub message_ids: Vec<String>,
}

impl FleetInboxHint {
    /// Parse and canonicalize a wake hint. Rejects unknown fields, the wrong
    /// URI, empty or oversized ID lists, and non-canonical IDs.
    pub fn parse(params: Value) -> Option<Self> {
        let mut hint = serde_json::from_value::<Self>(params).ok()?;
        hint.canonicalize().then_some(hint)
    }

    /// Sort/dedup IDs in place and return whether the hint is wire-valid.
    pub fn canonicalize(&mut self) -> bool {
        self.message_ids.sort();
        self.message_ids.dedup();
        if self.uri != FLEET_INBOX_URI
            || self.message_ids.is_empty()
            || self.message_ids.len() > MAX_FLEET_INBOX_MESSAGE_IDS
            || self
                .message_ids
                .iter()
                .any(|id| !is_canonical_message_id(id))
        {
            return false;
        }
        true
    }
}

fn is_canonical_message_id(id: &str) -> bool {
    id.len() <= 19
        && !id.starts_with('0')
        && id.bytes().all(|byte| byte.is_ascii_digit())
        && id.parse::<i64>().is_ok_and(|id| id > 0)
}

/// Harness-owned host information returned by [`FLEET_HOST_INFO_REQUEST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FleetHostInfo {
    pub os: String,
    pub arch: String,
    pub capabilities: Vec<String>,
    pub restrictions: Vec<String>,
}

impl FleetHostInfo {
    pub fn new(
        os: impl Into<String>,
        arch: impl Into<String>,
        capabilities: Vec<String>,
        restrictions: Vec<String>,
    ) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
            capabilities,
            restrictions,
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "os": self.os,
            "arch": self.arch,
            "capabilities": self.capabilities,
            "restrictions": self.restrictions,
        })
    }
}

/// Client `experimental` map advertising this protocol.
pub fn client_experimental_capabilities() -> HashMap<String, Value> {
    HashMap::from([(
        FLEET_EXPERIMENTAL_CAPABILITY.to_string(),
        Value::Object(Default::default()),
    )])
}

#[cfg(test)]
mod tests;
