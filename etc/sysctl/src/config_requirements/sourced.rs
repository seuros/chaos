use std::collections::BTreeMap;

use chaos_ipc::protocol::ApprovalPolicy;

use super::types::AppsRequirementsToml;
use super::types::FeatureRequirementsToml;
use super::types::McpServerRequirement;
use super::types::NetworkRequirementsToml;
use super::types::RequirementSource;
use super::types::ResidencyRequirement;
use super::types::SandboxModeRequirement;
use super::types::WebSearchModeRequirement;
use crate::requirements_exec_policy::RequirementsExecPolicyToml;

/// Value paired with the requirement source it came from, for better error
/// messages.
#[derive(Debug, Clone, PartialEq)]
pub struct Sourced<T> {
    pub value: T,
    pub source: RequirementSource,
}

impl<T> Sourced<T> {
    pub fn new(value: T, source: RequirementSource) -> Self {
        Self { value, source }
    }
}

impl<T> std::ops::Deref for Sourced<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigRequirementsWithSources {
    pub allowed_approval_policies: Option<Sourced<Vec<ApprovalPolicy>>>,
    pub allowed_sandbox_modes: Option<Sourced<Vec<SandboxModeRequirement>>>,
    pub allowed_web_search_modes: Option<Sourced<Vec<WebSearchModeRequirement>>>,
    pub feature_requirements: Option<Sourced<FeatureRequirementsToml>>,
    pub mcp_servers: Option<Sourced<BTreeMap<String, McpServerRequirement>>>,
    pub apps: Option<Sourced<AppsRequirementsToml>>,
    pub rules: Option<Sourced<RequirementsExecPolicyToml>>,
    pub enforce_residency: Option<Sourced<ResidencyRequirement>>,
    pub network: Option<Sourced<NetworkRequirementsToml>>,
}
