use chaos_ipc::openai_models::ModelInfo;

use crate::config::ChatgptContextWindow;
use crate::config::Config;
use crate::config::OBSERVED_CHATGPT_AUTO_COMPACT_TOKEN_LIMIT;
use crate::config::OBSERVED_CHATGPT_CONTEXT_WINDOW_TOKENS;
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
    let use_observed_chatgpt_window = config.model_context_window.is_none()
        && config.chatgpt_context_window == ChatgptContextWindow::Observed400k
        && config.model_provider.is_openai()
        && model.slug == "gpt-5.6-sol";

    if let Some(context_window) = config.model_context_window {
        model.context_window = Some(context_window);
    } else if use_observed_chatgpt_window {
        model.context_window = Some(OBSERVED_CHATGPT_CONTEXT_WINDOW_TOKENS);
    }
    if let Some(auto_compact_token_limit) = config.model_auto_compact_token_limit {
        model.auto_compact_token_limit = Some(auto_compact_token_limit);
    } else if use_observed_chatgpt_window {
        model.auto_compact_token_limit = Some(OBSERVED_CHATGPT_AUTO_COMPACT_TOKEN_LIMIT);
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

    // Merge provider-config and provider-derived native tools on top of the
    // ABI-derived ones (union, no duplicates). Providers that run web search
    // themselves must own it for every model they serve, catalogued or not, so
    // that the client-managed `web_search` tool with its `external_web_access`
    // argument is never injected alongside the provider's own.
    let provider_tools = config
        .model_provider
        .native_server_side_tools
        .iter()
        .cloned()
        .chain(
            crate::model_provider_info::native_server_side_tools_for_url(
                config.model_provider.base_url.as_deref(),
            ),
        );
    for tool in provider_tools {
        if !model.native_server_side_tools.contains(&tool) {
            model.native_server_side_tools.push(tool);
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
