use super::manager::ModelsManager;
use crate::config::Config;
use crate::function_tool::FunctionCallError;
use chaos_ipc::openai_models::{ModelPreset, ReasoningEffort, ReasoningEffortPreset};
use std::fmt::Display;

#[cfg(test)]
mod tests;

impl ModelsManager {
    pub(crate) async fn apply_provider_binding(
        &self,
        config: &mut Config,
        provider_id: &str,
        requested_model: Option<&str>,
        requested_effort: Option<ReasoningEffort>,
    ) -> Result<(), FunctionCallError> {
        let provider = config.model_providers.get(provider_id).ok_or_else(|| {
            FunctionCallError::RespondToModel(format!("Unknown model provider `{provider_id}`"))
        })?;
        let models = self
            .usable_cached_models_for_provider(provider_id, provider)
            .await
            .map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "Cannot bind to model provider `{provider_id}`: {err}"
                ))
            })?;
        let selected = match requested_model {
            Some(model) => find_model(
                &models,
                model,
                format_args!("model provider `{provider_id}`"),
            )?,
            None => models
                .iter()
                .find(|model| model.is_default)
                .or_else(|| models.first())
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(format!(
                        "Model provider `{provider_id}` has no usable cached models"
                    ))
                })?,
        };
        config.model_reasoning_effort = resolve_reasoning_effort(
            &selected.model,
            &selected.supported_reasoning_efforts,
            requested_effort,
            Some(selected.default_reasoning_effort),
        )?;
        config.model_provider = provider.clone();
        config.model_provider_id = provider_id.into();
        config.model = Some(selected.model.clone());
        Ok(())
    }
}

pub(crate) fn find_model<'a>(
    models: &'a [ModelPreset],
    requested: &str,
    context: impl Display,
) -> Result<&'a ModelPreset, FunctionCallError> {
    let normalized = requested.trim();
    models
        .iter()
        // Prefer an exact catalog slug before trying normalized input, since
        // providers can advertise distinct case-sensitive model names.
        .find(|model| model.model == requested)
        .or_else(|| models.iter().find(|model| model.model == normalized))
        .or_else(|| {
            let mut matches = models
                .iter()
                .filter(|model| model.model.trim().eq_ignore_ascii_case(normalized));
            let selected = matches.next()?;
            // Never resolve an ambiguous normalized id by catalog order.
            matches.next().is_none().then_some(selected)
        })
        .ok_or_else(|| {
            let available = models
                .iter()
                .map(|model| model.model.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            FunctionCallError::RespondToModel(format!(
                "Unknown model `{requested}` for {context}. Available models: {available}"
            ))
        })
}

pub(crate) fn resolve_reasoning_effort(
    model: &str,
    supported: &[ReasoningEffortPreset],
    requested: Option<ReasoningEffort>,
    default: Option<ReasoningEffort>,
) -> Result<Option<ReasoningEffort>, FunctionCallError> {
    let Some(requested) = requested else {
        return Ok(default);
    };
    if supported.iter().any(|preset| preset.effort == requested) {
        return Ok(Some(requested));
    }
    let supported = supported
        .iter()
        .map(|preset| preset.effort.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(FunctionCallError::RespondToModel(format!(
        "Reasoning effort `{requested}` is not supported for model `{model}`. \
         Supported reasoning efforts: {supported}"
    )))
}
