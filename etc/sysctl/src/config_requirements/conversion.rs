use std::collections::BTreeMap;

use chaos_ipc::protocol::ApprovalPolicy;
use serde::Deserialize;

use super::sourced::ConfigRequirementsWithSources;
use super::sourced::Sourced;
use super::types::AppsRequirementsToml;
use super::types::FeatureRequirementsToml;
use super::types::McpServerRequirement;
use super::types::NetworkRequirementsToml;
use super::types::RequirementSource;
use super::types::ResidencyRequirement;
use super::types::SandboxModeRequirement;
use super::types::WebSearchModeRequirement;
use super::validation::merge_enablement_settings_descending;
use crate::requirements_exec_policy::RequirementsExecPolicyToml;

/// Base config deserialized from system `requirements.toml` or MDM.
#[derive(Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ConfigRequirementsToml {
    pub allowed_approval_policies: Option<Vec<ApprovalPolicy>>,
    pub allowed_sandbox_modes: Option<Vec<SandboxModeRequirement>>,
    pub allowed_web_search_modes: Option<Vec<WebSearchModeRequirement>>,
    #[serde(rename = "features")]
    pub feature_requirements: Option<FeatureRequirementsToml>,
    pub mcp_servers: Option<BTreeMap<String, McpServerRequirement>>,
    pub apps: Option<AppsRequirementsToml>,
    pub rules: Option<RequirementsExecPolicyToml>,
    pub enforce_residency: Option<ResidencyRequirement>,
    #[serde(rename = "experimental_network")]
    pub network: Option<NetworkRequirementsToml>,
}

impl ConfigRequirementsToml {
    pub fn is_empty(&self) -> bool {
        self.allowed_approval_policies.is_none()
            && self.allowed_sandbox_modes.is_none()
            && self.allowed_web_search_modes.is_none()
            && self
                .feature_requirements
                .as_ref()
                .is_none_or(FeatureRequirementsToml::is_empty)
            && self.mcp_servers.is_none()
            && self
                .apps
                .as_ref()
                .is_none_or(AppsRequirementsToml::is_empty)
            && self.rules.is_none()
            && self.enforce_residency.is_none()
            && self.network.is_none()
    }
}

impl ConfigRequirementsWithSources {
    pub fn merge_unset_fields(&mut self, source: RequirementSource, other: ConfigRequirementsToml) {
        // For every field in `other` that is `Some`, if the corresponding field
        // in `self` is `None`, copy the value from `other` into `self`.
        macro_rules! fill_missing_take {
            ($base:expr, $other:expr, $source:expr, { $($field:ident),+ $(,)? }) => {
                $(
                    if $base.$field.is_none()
                        && let Some(value) = $other.$field.take()
                    {
                        $base.$field = Some(Sourced::new(value, $source.clone()));
                    }
                )+
            };
        }

        // Destructure without `..` so adding fields to `ConfigRequirementsToml`
        // forces this merge logic to be updated.
        let ConfigRequirementsToml {
            allowed_approval_policies: _,
            allowed_sandbox_modes: _,
            allowed_web_search_modes: _,
            feature_requirements: _,
            mcp_servers: _,
            apps: _,
            rules: _,
            enforce_residency: _,
            network: _,
        } = &other;

        let mut other = other;
        fill_missing_take!(
            self,
            other,
            source,
            {
                allowed_approval_policies,
                allowed_sandbox_modes,
                allowed_web_search_modes,
                feature_requirements,
                mcp_servers,
                rules,
                enforce_residency,
                network,
            }
        );

        if let Some(incoming_apps) = other.apps.take() {
            if let Some(existing_apps) = self.apps.as_mut() {
                merge_enablement_settings_descending(&mut existing_apps.value, incoming_apps);
            } else {
                self.apps = Some(Sourced::new(incoming_apps, source));
            }
        }
    }

    pub fn into_toml(self) -> ConfigRequirementsToml {
        let ConfigRequirementsWithSources {
            allowed_approval_policies,
            allowed_sandbox_modes,
            allowed_web_search_modes,
            feature_requirements,
            mcp_servers,
            apps,
            rules,
            enforce_residency,
            network,
        } = self;
        ConfigRequirementsToml {
            allowed_approval_policies: allowed_approval_policies.map(|sourced| sourced.value),
            allowed_sandbox_modes: allowed_sandbox_modes.map(|sourced| sourced.value),
            allowed_web_search_modes: allowed_web_search_modes.map(|sourced| sourced.value),
            feature_requirements: feature_requirements.map(|sourced| sourced.value),
            mcp_servers: mcp_servers.map(|sourced| sourced.value),
            apps: apps.map(|sourced| sourced.value),
            rules: rules.map(|sourced| sourced.value),
            enforce_residency: enforce_residency.map(|sourced| sourced.value),
            network: network.map(|sourced| sourced.value),
        }
    }
}
