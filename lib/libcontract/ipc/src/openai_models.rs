//! Shared model metadata types exchanged between Chaos services and clients.
//!
//! These types are serialized across core, TUI, app-server, and SDK boundaries, so field defaults
//! are used to preserve compatibility when older payloads omit newly introduced attributes.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use strum_macros::EnumIter;
use tracing::debug;

use crate::config_types::Personality;
use crate::config_types::ReasoningSummary;
use crate::config_types::Verbosity;

const PERSONALITY_PLACEHOLDER: &str = "{{ personality }}";

/// See https://platform.openai.com/docs/guides/reasoning?api-mode=responses#get-started-with-reasoning
#[derive(
    Debug,
    Serialize,
    Deserialize,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Display,
    JsonSchema,
    EnumIter,
    Hash,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    #[default]
    Medium,
    High,
    XHigh,
    Max,
    Ultra,
}

/// Canonical user-input modality tags advertised by a model.
#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Display, JsonSchema, EnumIter, Hash,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum InputModality {
    /// Plain text turns and tool payloads.
    Text,
    /// Image attachments included in user turns.
    Image,
}

/// Backward-compatible default when `input_modalities` is omitted on the wire.
///
/// Legacy payloads predate modality metadata, so we conservatively assume both text and images are
/// accepted unless a preset explicitly narrows support.
pub fn default_input_modalities() -> Vec<InputModality> {
    vec![InputModality::Text, InputModality::Image]
}

/// Canonical model-family metadata supplied explicitly by a provider or catalog.
///
/// `unknown` is deliberately conservative: callers must not infer a family from
/// a model slug, display name, endpoint URL, or wire protocol when enforcing
/// reviewer diversity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, JsonSchema)]
#[serde(transparent)]
pub struct ModelFamily(String);

impl ModelFamily {
    pub const UNKNOWN: &'static str = "unknown";

    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let value = value.trim().to_ascii_lowercase();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            Self::default()
        } else {
            Self(value)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_unknown(&self) -> bool {
        self.0 == Self::UNKNOWN
    }
}

impl<'de> Deserialize<'de> for ModelFamily {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(Self::new)
    }
}

impl Default for ModelFamily {
    fn default() -> Self {
        Self(Self::UNKNOWN.to_string())
    }
}

/// A reasoning effort option that can be surfaced for a model.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ReasoningEffortPreset {
    /// Effort level that the model supports.
    pub effort: ReasoningEffort,
    /// Short human description shown next to the effort in UIs.
    pub description: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub struct ModelAvailabilityNux {
    pub message: String,
}

/// Metadata describing a Chaos-supported model.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq)]
pub struct ModelPreset {
    /// Stable identifier for the preset.
    pub id: String,
    /// Model slug (e.g., "gpt-5").
    pub model: String,
    /// Explicit canonical family used for reviewer-diversity decisions.
    #[serde(default)]
    pub model_family: ModelFamily,
    /// Display name shown in UIs.
    pub display_name: String,
    /// Short human description shown in UIs.
    pub description: String,
    /// Reasoning effort applied when none is explicitly chosen.
    pub default_reasoning_effort: ReasoningEffort,
    /// Supported reasoning effort options.
    pub supported_reasoning_efforts: Vec<ReasoningEffortPreset>,
    /// Whether this model supports personality-specific instructions.
    #[serde(default)]
    pub supports_personality: bool,
    /// Whether this is the default model for new users.
    pub is_default: bool,
    /// Whether this preset should appear in the picker UI.
    pub show_in_picker: bool,
    /// Availability NUX shown when this preset becomes accessible to the user.
    pub availability_nux: Option<ModelAvailabilityNux>,
    /// whether this model is supported in the api
    pub supported_in_api: bool,
    /// Input modalities accepted when composing user turns for this preset.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
}

/// Visibility of a model in the picker or APIs.
#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, JsonSchema, EnumIter, Display,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ModelVisibility {
    List,
    Hide,
    None,
}

/// Shell execution capability for a model.
#[derive(
    Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, JsonSchema, EnumIter, Display, Hash,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ConfigShellToolType {
    Default,
    Local,
    UnifiedExec,
    Disabled,
    ShellCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApplyPatchToolType {
    Freeform,
    Function,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchToolType {
    #[default]
    Text,
    TextAndImage,
}

/// Server-provided truncation policy metadata for a model.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TruncationMode {
    Bytes,
    Tokens,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, JsonSchema)]
pub struct TruncationPolicyConfig {
    pub mode: TruncationMode,
    pub limit: i64,
}

impl TruncationPolicyConfig {
    pub const fn bytes(limit: i64) -> Self {
        Self {
            mode: TruncationMode::Bytes,
            limit,
        }
    }

    pub const fn tokens(limit: i64) -> Self {
        Self {
            mode: TruncationMode::Tokens,
            limit,
        }
    }
}

/// Semantic version triple encoded as an array in JSON (e.g. [0, 62, 0]).
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, JsonSchema)]
pub struct ClientVersion(pub i32, pub i32, pub i32);

const fn default_effective_context_window_percent() -> i64 {
    95
}

/// Model metadata returned by the Chaos backend `/models` endpoint.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema)]
pub struct ModelInfo {
    pub slug: String,
    /// Explicit canonical family supplied by catalog/provider metadata.
    #[serde(default)]
    pub model_family: ModelFamily,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_reasoning_level: Option<ReasoningEffort>,
    pub supported_reasoning_levels: Vec<ReasoningEffortPreset>,
    pub shell_type: ConfigShellToolType,
    pub visibility: ModelVisibility,
    pub supported_in_api: bool,
    pub priority: i32,
    pub availability_nux: Option<ModelAvailabilityNux>,
    pub base_instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_messages: Option<ModelMessages>,
    pub supports_reasoning_summaries: bool,
    #[serde(default)]
    pub default_reasoning_summary: ReasoningSummary,
    pub support_verbosity: bool,
    pub default_verbosity: Option<Verbosity>,
    pub apply_patch_tool_type: Option<ApplyPatchToolType>,
    #[serde(default)]
    pub web_search_tool_type: WebSearchToolType,
    pub truncation_policy: TruncationPolicyConfig,
    pub supports_parallel_tool_calls: bool,
    #[serde(default)]
    pub supports_image_detail_original: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    /// Token threshold for automatic compaction. When omitted, core derives it
    /// from `context_window` (90%). When provided, core clamps it to 90% of the
    /// context window when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_token_limit: Option<i64>,
    /// Percentage of the context window considered usable for inputs, after
    /// reserving headroom for system prompts, tool overhead, and model output.
    #[serde(default = "default_effective_context_window_percent")]
    pub effective_context_window_percent: i64,
    pub experimental_supported_tools: Vec<String>,
    /// Input modalities accepted by the backend for this model.
    #[serde(default = "default_input_modalities")]
    pub input_modalities: Vec<InputModality>,
    /// Server-side tools the provider handles natively (e.g. `web_search`, `x_search`).
    /// These are injected into the request tools array as bare `{ "type": "<name>" }` entries
    /// without any function schema — the provider executes them on its infrastructure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub native_server_side_tools: Vec<String>,
    /// Internal-only marker set by core when a model slug resolved to fallback metadata.
    #[serde(default, skip_serializing, skip_deserializing)]
    #[schemars(skip)]
    pub used_fallback_model_metadata: bool,
}

impl ModelInfo {
    pub fn auto_compact_token_limit(&self) -> Option<i64> {
        let context_limit = self
            .context_window
            .map(|context_window| (context_window * 9) / 10);
        let config_limit = self.auto_compact_token_limit;
        if let Some(context_limit) = context_limit {
            return Some(
                config_limit.map_or(context_limit, |limit| std::cmp::min(limit, context_limit)),
            );
        }
        config_limit
    }

    pub fn supports_personality(&self) -> bool {
        self.model_messages
            .as_ref()
            .is_some_and(ModelMessages::supports_personality)
    }

    pub fn get_model_instructions(&self, personality: Option<Personality>) -> String {
        if let Some(model_messages) = &self.model_messages
            && let Some(template) = &model_messages.instructions_template
        {
            // if we have a template, always use it
            let personality_message = model_messages
                .get_personality_message(personality)
                .unwrap_or_default();
            template.replace(PERSONALITY_PLACEHOLDER, personality_message.as_str())
        } else if let Some(personality) = personality {
            debug!(
                model = %self.slug,
                %personality,
                "model has no personality template, using base instructions"
            );
            self.base_instructions.clone()
        } else {
            self.base_instructions.clone()
        }
    }
}

/// A strongly-typed template for assembling model instructions and developer messages. If
/// instructions_* is populated and valid, it will override base_instructions.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema)]
pub struct ModelMessages {
    pub instructions_template: Option<String>,
    pub instructions_variables: Option<ModelInstructionsVariables>,
}

impl ModelMessages {
    fn has_personality_placeholder(&self) -> bool {
        self.instructions_template
            .as_ref()
            .map(|spec| spec.contains(PERSONALITY_PLACEHOLDER))
            .unwrap_or(false)
    }

    fn supports_personality(&self) -> bool {
        self.has_personality_placeholder()
            && self
                .instructions_variables
                .as_ref()
                .is_some_and(ModelInstructionsVariables::is_complete)
    }

    pub fn get_personality_message(&self, personality: Option<Personality>) -> Option<String> {
        self.instructions_variables
            .as_ref()
            .and_then(|variables| variables.get_personality_message(personality))
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema)]
pub struct ModelInstructionsVariables {
    pub personality_default: Option<String>,
    pub personality_friendly: Option<String>,
    pub personality_pragmatic: Option<String>,
}

impl ModelInstructionsVariables {
    pub fn is_complete(&self) -> bool {
        self.personality_default.is_some()
            && self.personality_friendly.is_some()
            && self.personality_pragmatic.is_some()
    }

    pub fn get_personality_message(&self, personality: Option<Personality>) -> Option<String> {
        if let Some(personality) = personality {
            match personality {
                Personality::None => Some(String::new()),
                Personality::Friendly => self.personality_friendly.clone(),
                Personality::Pragmatic => self.personality_pragmatic.clone(),
            }
        } else {
            self.personality_default.clone()
        }
    }
}

/// Response wrapper for `/models`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, JsonSchema, Default)]
pub struct ModelsResponse {
    pub models: Vec<ModelInfo>,
}

// convert ModelInfo to ModelPreset
impl From<ModelInfo> for ModelPreset {
    fn from(info: ModelInfo) -> Self {
        let supports_personality = info.supports_personality();
        ModelPreset {
            id: info.slug.clone(),
            model: info.slug.clone(),
            model_family: info.model_family,
            display_name: info.display_name,
            description: info.description.unwrap_or_default(),
            default_reasoning_effort: info
                .default_reasoning_level
                .unwrap_or(ReasoningEffort::None),
            supported_reasoning_efforts: info.supported_reasoning_levels.clone(),
            supports_personality,
            is_default: false, // default is the highest priority available model
            show_in_picker: info.visibility == ModelVisibility::List,
            availability_nux: info.availability_nux,
            supported_in_api: info.supported_in_api,
            input_modalities: info.input_modalities,
        }
    }
}

impl ModelPreset {
    /// Filter models based on authentication mode.
    ///
    /// In ChatGPT mode, all models are visible. Otherwise, only API-supported models are shown.
    pub fn filter_by_auth(models: Vec<ModelPreset>, chatgpt_mode: bool) -> Vec<ModelPreset> {
        models
            .into_iter()
            .filter(|model| chatgpt_mode || model.supported_in_api)
            .collect()
    }

    /// Recompute the single default preset using picker visibility.
    ///
    /// The first picker-visible model wins; if none are picker-visible, the first model wins.
    pub fn mark_default_by_picker_visibility(models: &mut [ModelPreset]) {
        for preset in models.iter_mut() {
            preset.is_default = false;
        }
        if let Some(default) = models.iter_mut().find(|preset| preset.show_in_picker) {
            default.is_default = true;
        } else if let Some(default) = models.first_mut() {
            default.is_default = true;
        }
    }
}

#[cfg(test)]
mod tests;
