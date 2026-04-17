use chaos_ipc::openai_models::ModelInfo;

use crate::config::Config;
use crate::truncate::approx_bytes_for_tokens;

// Re-export pure ABI conversion from the catalog crate.
pub use chaos_model_catalog::model_info_from_abi;

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
        use chaos_ipc::openai_models::TruncationMode;
        model.truncation_policy = match model.truncation_policy.mode {
            TruncationMode::Bytes => {
                use chaos_ipc::openai_models::TruncationPolicyConfig;
                let byte_limit =
                    i64::try_from(approx_bytes_for_tokens(token_limit)).unwrap_or(i64::MAX);
                TruncationPolicyConfig::bytes(byte_limit)
            }
            TruncationMode::Tokens => {
                use chaos_ipc::openai_models::TruncationPolicyConfig;
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

#[cfg(test)]
#[path = "model_info_tests.rs"]
mod tests;
