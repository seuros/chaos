//! Validated transport observations, shared by live updates and journal replay.

use std::collections::HashMap;

use chaos_ipc::protocol::ModelContextWindowScope;
use chaos_ipc::protocol::ObservedModelContextWindowItem;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ModelBinding {
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) scope: ModelContextWindowScope,
}

#[derive(Debug, Clone)]
pub(crate) struct ContextWindowObservation {
    binding: ModelBinding,
    context_window: i64,
}

impl ContextWindowObservation {
    pub(crate) fn new(binding: ModelBinding, context_window: i64) -> Option<Self> {
        (context_window > 0
            && !binding.provider.trim().is_empty()
            && !binding.model.trim().is_empty()
            && !binding.scope.wire_api.trim().is_empty()
            && !binding.scope.base_url.trim().is_empty())
        .then_some(Self {
            binding,
            context_window,
        })
    }

    pub(crate) fn from_record(record: &ObservedModelContextWindowItem) -> Option<Self> {
        Self::new(
            ModelBinding {
                provider: record.provider.clone(),
                model: record.model.clone(),
                scope: record.scope.clone(),
            },
            record.context_window,
        )
    }

    pub(crate) fn to_record(&self) -> ObservedModelContextWindowItem {
        ObservedModelContextWindowItem {
            provider: self.binding.provider.clone(),
            model: self.binding.model.clone(),
            context_window: self.context_window,
            scope: self.binding.scope.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct ContextWindows(HashMap<ModelBinding, i64>);

impl ContextWindows {
    pub(crate) fn window(&self, binding: &ModelBinding) -> Option<i64> {
        self.0.get(binding).copied()
    }

    pub(crate) fn contains(&self, observation: &ContextWindowObservation) -> bool {
        self.window(&observation.binding) == Some(observation.context_window)
    }

    pub(crate) fn record(&mut self, observation: &ContextWindowObservation) {
        self.0
            .insert(observation.binding.clone(), observation.context_window);
    }
}

#[cfg(test)]
mod tests;
