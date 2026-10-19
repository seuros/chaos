//! # chaos-clamp
//!
//! First-party model CLI subprocess transports for Chaos.
//!
//! Claude Code is driven through its bidirectional stream-JSON control
//! protocol. Antigravity uses the official OAuth-authenticated `agy` CLI with
//! native tools denied and the session-scoped Chaos MCP bridge as its action
//! surface.

mod antigravity;
mod antigravity_sandbox;
mod egress;
mod protocol;
mod proxy;
mod transport;

pub use antigravity::AntigravityBridgeConfig;
pub use antigravity::AntigravityConfig;
pub use antigravity::AntigravityConversationStore;
pub use antigravity::AntigravityEgress;
pub use antigravity::AntigravityError;
pub use antigravity::AntigravityEvent;
pub use antigravity::AntigravityInit;
pub use antigravity::AntigravityResult;
pub use antigravity::AntigravitySandbox;
pub use antigravity::AntigravityStepUpdate;
pub use antigravity::AntigravityTransport;
pub use antigravity::AntigravityTurn;
pub use antigravity::AntigravityUsage;
pub use antigravity_sandbox::prepare_antigravity_sandbox;
pub use egress::ANTIGRAVITY_ALLOWED_HOSTS;
pub use egress::EgressPolicy;
pub use egress::EgressProxy;
pub use protocol::ControlRequest;
pub use protocol::ControlResponse;
pub use protocol::Message;
pub use protocol::Usage;
pub use proxy::FileWiretapSink;
pub use proxy::WiretapExchange;
pub use proxy::WiretapProxy;
pub use proxy::WiretapSink;
pub use transport::ClampConfig;
pub use transport::ClampError;
pub use transport::ClampInfo;
pub use transport::ClampTransport;
pub use transport::HookCallbackHandler;
pub use transport::McpMessageHandler;
pub use transport::ToolPermissionHandler;

use std::sync::Mutex;

/// Cached model list from Claude Code init response.
static CACHED_MODELS: Mutex<Option<serde_json::Value>> = Mutex::new(None);

/// Store the model list from the Claude Code init response.
pub fn set_cached_models(models: serde_json::Value) {
    if let Ok(mut guard) = CACHED_MODELS.lock() {
        *guard = Some(models);
    }
}

/// Get the cached Claude Code model list.
pub fn cached_models() -> Option<serde_json::Value> {
    CACHED_MODELS.lock().ok().and_then(|g| g.clone())
}

/// Model presets built from the cached Claude Code init response.
///
/// Empty until the first clamped turn has initialized a Claude Code
/// subprocess in this process.
pub fn cached_model_presets() -> Vec<chaos_ipc::openai_models::ModelPreset> {
    cached_models()
        .map(|models| model_presets_from_init(&models))
        .unwrap_or_default()
}

/// Whether `model` is a value Claude Code advertised in its init response
/// (for example the `haiku` / `sonnet` / `opus` aliases), excluding the
/// `default` sentinel, which means "keep the subscription default".
pub fn is_cached_model(model: &str) -> bool {
    model != "default"
        && cached_model_presets()
            .iter()
            .any(|preset| preset.model == model)
}

/// Build model presets from the `models` array of a Claude Code init
/// response. Entries without a string `value` are skipped.
pub fn model_presets_from_init(
    models: &serde_json::Value,
) -> Vec<chaos_ipc::openai_models::ModelPreset> {
    use chaos_ipc::openai_models::ModelPreset;
    use chaos_ipc::openai_models::ReasoningEffort;
    use chaos_ipc::openai_models::ReasoningEffortPreset;

    let Some(models) = models.as_array() else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|m| {
            let value = m.get("value").and_then(|v| v.as_str())?;
            let display = m
                .get("displayName")
                .and_then(|v| v.as_str())
                .unwrap_or(value);
            let desc = m.get("description").and_then(|v| v.as_str()).unwrap_or("");
            let efforts: Vec<ReasoningEffortPreset> = m
                .get("supportedEffortLevels")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| {
                            let s = e.as_str()?;
                            let effort = match s {
                                "low" => ReasoningEffort::Low,
                                "medium" => ReasoningEffort::Medium,
                                "high" => ReasoningEffort::High,
                                _ => return None,
                            };
                            Some(ReasoningEffortPreset {
                                effort,
                                description: s.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(ModelPreset {
                id: value.to_string(),
                model: value.to_string(),
                model_family: Default::default(),
                display_name: display.to_string(),
                description: desc.to_string(),
                default_reasoning_effort: ReasoningEffort::Medium,
                supported_reasoning_efforts: efforts,
                supports_personality: false,
                is_default: value == "default",
                show_in_picker: true,
                availability_nux: None,
                supported_in_api: true,
                input_modalities: vec![],
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
