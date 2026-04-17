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

// 4 bytes per token approximation, same constant as kern's truncate module.
const APPROX_BYTES_PER_TOKEN: i64 = 4;

/// Convert a provider-neutral `AbiModelInfo` into `ModelInfo`.
///
/// Produces catalog metadata only — `base_instructions` is left empty and must
/// be filled in by the kern-side `with_config_overrides` finalizer before
/// the model is used in a session.
pub fn model_info_from_abi(abi: &AbiModelInfo) -> ModelInfo {
    let input_modalities = if abi.supports_images {
        vec![InputModality::Text, InputModality::Image]
    } else {
        vec![InputModality::Text]
    };

    let context_window = abi.max_input_tokens;
    let truncation_limit = context_window
        .map(|tokens| tokens.saturating_mul(APPROX_BYTES_PER_TOKEN))
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
        // Left empty; kern's with_config_overrides always provides the real value.
        base_instructions: String::new(),
        model_messages: None,
        supports_reasoning_summaries: abi.supports_thinking,
        default_reasoning_summary: if abi.supports_thinking {
            ReasoningSummary::Auto
        } else {
            ReasoningSummary::None
        },
        support_verbosity: false,
        default_verbosity: None,
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
