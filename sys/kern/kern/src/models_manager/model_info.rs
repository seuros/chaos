use chaos_abi::AbiModelInfo;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::openai_models::ApplyPatchToolType;
use chaos_ipc::openai_models::ConfigShellToolType;
use chaos_ipc::openai_models::InputModality;
use chaos_ipc::openai_models::ModelInfo;
use chaos_ipc::openai_models::ModelVisibility;
use chaos_ipc::openai_models::TruncationMode;
use chaos_ipc::openai_models::TruncationPolicyConfig;
use chaos_ipc::openai_models::WebSearchToolType;

use crate::config::Config;
use crate::truncate::approx_bytes_for_tokens;

pub const BASE_INSTRUCTIONS: &str = include_str!("../../prompt.md");

pub(crate) fn with_config_overrides(mut model: ModelInfo, config: &Config) -> ModelInfo {
    if let Some(supports_reasoning_summaries) = config.model_supports_reasoning_summaries
        && supports_reasoning_summaries
    {
        model.supports_reasoning_summaries = true;
    }
    if let Some(context_window) = config.model_context_window {
        model.context_window = Some(context_window);
    }
    if let Some(auto_compact_token_limit) = config.model_auto_compact_token_limit {
        model.auto_compact_token_limit = Some(auto_compact_token_limit);
    }
    if let Some(token_limit) = config.tool_output_token_limit {
        model.truncation_policy = match model.truncation_policy.mode {
            TruncationMode::Bytes => {
                let byte_limit =
                    i64::try_from(approx_bytes_for_tokens(token_limit)).unwrap_or(i64::MAX);
                TruncationPolicyConfig::bytes(byte_limit)
            }
            TruncationMode::Tokens => {
                let limit = i64::try_from(token_limit).unwrap_or(i64::MAX);
                TruncationPolicyConfig::tokens(limit)
            }
        };
    }

    // Merge provider-config native tools on top of ABI-derived ones (union, no duplicates).
    for tool in &config.model_provider.native_server_side_tools {
        if !model.native_server_side_tools.contains(tool) {
            model.native_server_side_tools.push(tool.clone());
        }
    }

    if let Some(base_instructions) = &config.base_instructions {
        model.base_instructions = base_instructions.clone();
        model.model_messages = None;
    } else {
        // Always override server-supplied instructions with the local prompt.
        // The server sends OpenAI-branded personality; ChaOS has its own identity.
        model.base_instructions = BASE_INSTRUCTIONS.to_string();
        model.model_messages = None;
    }

    model
}

/// Convert a provider-neutral `AbiModelInfo` into kern's `ModelInfo`.
///
/// Fills in sensible defaults for fields that the ABI does not carry.
/// The resulting `ModelInfo` is not flagged as fallback metadata.
pub(crate) fn model_info_from_abi(abi: &AbiModelInfo) -> ModelInfo {
    let input_modalities = if abi.supports_images {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    };

    let context_window = abi.max_input_tokens;
    let truncation_limit = context_window
        .and_then(|tokens| usize::try_from(tokens).ok())
        .map(|tokens| approx_bytes_for_tokens(tokens) as i64)
        .unwrap_or(10_000);

    ModelInfo {
        slug: abi.id.clone(),
        display_name: abi.display_name.clone(),
        description: None,
        default_reasoning_level: None,
        supported_reasoning_levels: Vec::new(),
        shell_type: ConfigShellToolType::Default,
        visibility: ModelVisibility::List,
        supported_in_api: true,
        priority: 50,
        availability_nux: None,
        base_instructions: BASE_INSTRUCTIONS.to_string(),
        model_messages: None,
        supports_reasoning_summaries: abi.supports_thinking,
        default_reasoning_summary: if abi.supports_thinking {
            ReasoningSummary::Auto
        } else {
            ReasoningSummary::None
        },
        support_verbosity: false,
        default_verbosity: None,
        // Unknown models get the portable JSON tool variant. `Freeform` emits
        // `type: "custom"` on the wire, which is an OpenAI-Responses-only
        // extension — Responses clones (xAI, DeepSeek, etc.) reject it with
        // 422. Catalog entries can still opt into `Freeform` explicitly.
        apply_patch_tool_type: Some(ApplyPatchToolType::Function),
        web_search_tool_type: WebSearchToolType::Text,
        truncation_policy: TruncationPolicyConfig {
            mode: TruncationMode::Bytes,
            limit: truncation_limit,
        },
        supports_parallel_tool_calls: true,
        supports_image_detail_original: abi.supports_images,
        context_window,
        auto_compact_token_limit: context_window.map(|w| w * 80 / 100),
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities,
        native_server_side_tools: abi.native_server_side_tools.clone(),
        used_fallback_model_metadata: false,
    }
}

#[cfg(test)]
#[path = "model_info_tests.rs"]
mod tests;
