//! Stateless inference on the configured providers, for callers outside a
//! session (the `/inference` lane of `chaos serve`). The caller owns the
//! conversation, the instructions and the tools; nothing here runs a tool or
//! records history.

use std::sync::Arc;

use chaos_ipc::ProcessId;
use chaos_ipc::config_types::ReasoningSummary;
use chaos_ipc::dynamic_tools::DynamicToolSpec;
use chaos_ipc::models::BaseInstructions;
use chaos_ipc::models::ResponseItem;
use chaos_ipc::openai_models::ReasoningEffort;
use chaos_ipc::protocol::SessionSource;
use chaos_snitch::SessionTelemetry;
use serde_json::Value;

use crate::AuthManager;
use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseStream;
use crate::client_common::tools::ToolSpec;
use crate::config::Config;
use crate::models_manager::RefreshStrategy;
use crate::process_table::ProcessTable;
use crate::tools::spec::dynamic_tool_to_model_tool;

pub struct InferenceInput {
    /// Groups the provider calls of one conversation (the trip id).
    pub conversation_id: ProcessId,
    /// A configured provider id; `None` uses the configured default.
    pub provider_id: Option<String>,
    pub model: String,
    pub instructions: String,
    pub input: Vec<ResponseItem>,
    pub tools: Vec<DynamicToolSpec>,
    pub parallel_tool_calls: bool,
    pub effort: Option<ReasoningEffort>,
    pub summary: Option<ReasoningSummary>,
    pub output_schema: Option<Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error("{0}")]
    UnknownTarget(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error(transparent)]
    Provider(#[from] crate::error::ChaosErr),
}

/// The model the request resolved to, and the stream of its reply.
pub struct Inference {
    pub provider_id: String,
    pub model: String,
    pub stream: ResponseStream,
}

pub async fn infer(
    config: &Config,
    process_table: &ProcessTable,
    auth_manager: &Arc<AuthManager>,
    request: InferenceInput,
) -> Result<Inference, InferenceError> {
    let provider_id = request
        .provider_id
        .clone()
        .unwrap_or_else(|| config.model_provider_id.clone());
    let manager = process_table.get_models_manager();
    let provider = config
        .model_providers
        .get(&provider_id)
        .cloned()
        .ok_or_else(|| {
            InferenceError::UnknownTarget(format!("unknown provider `{provider_id}`"))
        })?;
    // Binding reads only the cached catalog. A session warms it when it lists
    // models; a stateless caller never does, so fetch it here when it is missing.
    if manager.is_bound_to(&provider_id, &provider) {
        manager.list_models(RefreshStrategy::OnlineIfUncached).await;
    } else if let Some(rebound) = manager.rebound_to(&provider_id, provider) {
        rebound.list_models(RefreshStrategy::OnlineIfUncached).await;
    }
    let mut config = config.clone();
    manager
        .apply_provider_binding(&mut config, &provider_id, Some(&request.model), None)
        .await
        .map_err(|error| InferenceError::UnknownTarget(error.to_string()))?;
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| request.model.clone());
    let info = manager.get_model_info(&model, &config).await;

    let tools = request
        .tools
        .iter()
        .map(|tool| dynamic_tool_to_model_tool(tool).map(ToolSpec::Function))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| InferenceError::InvalidRequest(format!("tool schema: {error}")))?;
    let prompt = Prompt {
        input: request.input,
        tools,
        parallel_tool_calls: request.parallel_tool_calls,
        base_instructions: BaseInstructions {
            text: request.instructions,
        },
        output_schema: request.output_schema,
        ..Prompt::default()
    };

    let client = ModelClient::new(
        Some(auth_manager.for_provider(&provider_id)),
        request.conversation_id,
        provider_id.clone(),
        config.model_provider.clone(),
        SessionSource::Api,
        config.permissions.approval_policy.value(),
        config.model_verbosity,
        true,
        None,
        false,
        config.clamp_settings(),
    );
    let telemetry = SessionTelemetry::new(
        request.conversation_id,
        &model,
        &info.slug,
        None,
        "chaos_httpd",
        false,
        "inference",
        SessionSource::Api,
    );
    let stream = client
        .new_session()
        .stream(
            &prompt,
            &info,
            &telemetry,
            request.effort.or(config.model_reasoning_effort),
            request
                .summary
                .or(config.model_reasoning_summary)
                .unwrap_or_default(),
            config.service_tier,
            None,
        )
        .await?;
    Ok(Inference {
        provider_id,
        model,
        stream,
    })
}
