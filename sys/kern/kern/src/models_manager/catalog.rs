//! User-facing identities span accounts; inference catalogs stay provider-scoped.
use super::manager::ModelsManager;
use crate::config::Config;
use crate::error::{ChaosErr, Result};
use chaos_ipc::openai_models::ModelPreset;
use rama::futures;
use rama::futures::StreamExt;
use std::collections::{HashMap, HashSet};

const SELECTION_ID_PREFIX: &str = "chaos:model:";

#[cfg(test)]
mod tests;

/// A picker identity, distinct from the model slug sent over the provider wire.
/// Tuple encoding keeps arbitrary configured account ids and slugs unambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSelection {
    pub provider_id: String,
    pub model: String,
}

impl ModelSelection {
    /// Bare model slugs always belong to the caller's current account.
    /// A malformed qualified id must never fall back to a bare slug.
    pub fn resolve(id: &str, current_provider_id: &str) -> Option<Self> {
        if Self::is_id(id) {
            Self::from_id(id)
        } else {
            Some(Self {
                provider_id: current_provider_id.to_string(),
                model: id.to_string(),
            })
        }
    }

    pub fn same_model(left: &str, right: &str, current_provider_id: &str) -> bool {
        Self::resolve(left, current_provider_id)
            .zip(Self::resolve(right, current_provider_id))
            .is_some_and(|(left, right)| left == right)
    }

    pub fn id(&self) -> String {
        format!(
            "{SELECTION_ID_PREFIX}{}",
            serde_json::json!([self.provider_id, self.model])
        )
    }

    pub fn is_id(id: &str) -> bool {
        id.starts_with(SELECTION_ID_PREFIX)
    }

    pub fn from_id(id: &str) -> Option<Self> {
        let [provider_id, model] =
            serde_json::from_str::<[String; 2]>(id.strip_prefix(SELECTION_ID_PREFIX)?).ok()?;
        Some(Self { provider_id, model })
    }

    /// Borrow ordinary wire slugs for rendering without allocating.
    pub fn wire_model(id: &str) -> &str {
        let Some(encoded) = id.strip_prefix(SELECTION_ID_PREFIX) else {
            return id;
        };
        serde_json::from_str::<[&str; 2]>(encoded)
            .map(|parts| parts[1])
            .unwrap_or(id)
    }
}

/// A user-facing snapshot shared by every model picker, never an inference catalog.
/// Normalization preserves provider order and keeps equal slugs on different accounts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelPickerCatalog {
    models: Vec<ModelPreset>,
}

impl ModelPickerCatalog {
    /// Also accepts externally supplied presets, such as a CLI bridge's catalog.
    /// Their selection ids remain unchanged.
    pub fn new(models: Vec<ModelPreset>) -> Self {
        let mut seen = HashSet::new();
        Self {
            models: models
                .into_iter()
                .filter_map(|mut preset| {
                    if !preset.show_in_picker {
                        return None;
                    }
                    if ModelSelection::is_id(&preset.id) {
                        preset.id = ModelSelection::from_id(&preset.id)?.id();
                    }
                    seen.insert(preset.id.clone()).then_some(preset)
                })
                .collect(),
        }
    }

    pub fn models(&self) -> &[ModelPreset] {
        &self.models
    }

    pub fn into_models(self) -> Vec<ModelPreset> {
        self.models
    }

    pub fn find(&self, id: &str, current_provider_id: &str) -> Option<&ModelPreset> {
        self.models
            .iter()
            .find(|preset| ModelSelection::same_model(id, &preset.id, current_provider_id))
    }
}

impl ModelsManager {
    /// Resolve binding metadata after the caller has validated a selection.
    /// Preserve live endpoint overrides instead of reconstructing them from the registry.
    pub fn provider_for_selection(
        &self,
        config: &Config,
        selection: &ModelSelection,
    ) -> Option<crate::ModelProviderInfo> {
        if selection.provider_id == self.provider_id() {
            Some(self.provider().clone())
        } else if selection.provider_id == config.model_provider_id {
            Some(config.model_provider.clone())
        } else {
            config.model_providers.get(&selection.provider_id).cloned()
        }
    }

    /// Materialize the inference catalog for an already validated binding.
    pub(crate) async fn for_config(&self, config: &Config) -> Self {
        if self.is_bound_to(&config.model_provider_id, &config.model_provider) {
            return self.clone();
        }
        let Some(target) =
            self.rebound_to(&config.model_provider_id, config.model_provider.clone())
        else {
            unreachable!("validated model selection must have a rebindable catalog");
        };
        target.load_cached_catalog().await;
        target
    }

    /// Credential-filtered choices shared by native UI and ACP. `id` identifies
    /// an account/model pair; `model` always remains a provider wire slug.
    pub async fn model_picker_catalog(
        &self,
        providers: &HashMap<String, crate::ModelProviderInfo>,
        active_provider_id: &str,
        refresh_strategy: super::RefreshStrategy,
    ) -> ModelPickerCatalog {
        let mut configured = providers.clone();
        // Runtime endpoint overrides belong to the bound account, even when
        // its registry entry still contains the installation's default URL.
        configured.insert(self.provider_id().to_string(), self.provider().clone());
        let providers = &configured;
        let groups = self
            .list_models_by_provider(providers, active_provider_id)
            .await;
        let models = futures::stream::iter(groups)
            .map(|mut group| async move {
                let provider = &providers[&group.provider_id];
                if !self.provider_is_usable(&group.provider_id, provider) {
                    group.models.clear();
                    return group;
                }
                let target = if self.is_bound_to(&group.provider_id, provider) {
                    self.clone()
                } else if let Some(target) = self.rebound_to(&group.provider_id, provider.clone()) {
                    target
                } else {
                    // An authoritative custom catalog cannot describe another account.
                    group.models.clear();
                    return group;
                };
                if group.models.is_empty() && refresh_strategy != super::RefreshStrategy::Offline {
                    group.models = target.list_models(refresh_strategy).await;
                }
                group.models = target.filter_available_presets(group.models);
                group
            })
            .buffered(4)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .flat_map(|group| {
                group.models.into_iter().map(move |mut preset| {
                    preset.id = ModelSelection {
                        provider_id: group.provider_id.clone(),
                        model: preset.model.clone(),
                    }
                    .id();
                    preset.display_name =
                        format!("{} · {}", preset.display_name, group.provider_id);
                    preset
                })
            })
            .collect::<Vec<_>>();
        let catalog = ModelPickerCatalog::new(models);
        *self.picker_catalog.write().await = catalog.clone();
        catalog
    }

    /// A saved choice that no longer resolves must not stop a session from
    /// starting: drop it so the active account's default applies, and hand
    /// back the notice to show.
    pub async fn resolve_model_selection_or_fallback(&self, config: &mut Config) -> Option<String> {
        let err = self.resolve_model_selection(config).await.err()?;
        let requested = config.model.take().unwrap_or_default();
        config.model_reasoning_effort = None;
        Some(format!(
            "Default model `{}` is unavailable ({err}); \
             falling back to the first available model on `{}`.",
            ModelSelection::wire_model(&requested),
            config.model_provider_id
        ))
    }

    /// Validate an explicit picker choice before changing configuration.
    /// Bare slugs retain their meaning: a model on the current account.
    pub async fn resolve_model_selection(&self, config: &mut Config) -> Result<()> {
        let Some(id) = config
            .model
            .as_deref()
            .filter(|id| ModelSelection::is_id(id))
        else {
            return Ok(());
        };
        let selection = ModelSelection::from_id(id)
            .ok_or_else(|| ChaosErr::InvalidRequest("malformed model selection id".into()))?;
        let catalog = self
            .model_picker_catalog(
                &config.model_providers,
                &config.model_provider_id,
                super::RefreshStrategy::Offline,
            )
            .await;
        let selected = catalog.find(id, &config.model_provider_id).ok_or_else(|| {
            ChaosErr::InvalidRequest("model/account choice is unavailable".into())
        })?;
        let provider = self
            .provider_for_selection(config, &selection)
            .ok_or_else(|| ChaosErr::InvalidRequest("model account is not configured".into()))?;
        config.model_provider_id = selection.provider_id;
        config.model_provider = provider;
        config.model = Some(selected.model.clone());
        config.model_reasoning_effort = config
            .model_reasoning_effort
            .filter(|effort| {
                selected
                    .supported_reasoning_efforts
                    .iter()
                    .any(|preset| preset.effort == *effort)
            })
            .or(Some(selected.default_reasoning_effort));
        Ok(())
    }
}
