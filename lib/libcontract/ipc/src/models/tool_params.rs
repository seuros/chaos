use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use super::permissions::PermissionProfile;
use super::permissions::SandboxPermissions;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct SearchToolCallParams {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<usize>,
}

/// If the `name` of a `ResponseItem::FunctionCall` is either `container.exec`
/// or `shell`, the `arguments` field should deserialize to this struct.
#[derive(Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct ShellToolCallParams {
    pub command: Vec<String>,
    pub workdir: Option<String>,

    /// This is the maximum time in milliseconds that the command is allowed to run.
    #[serde(alias = "timeout")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_permissions: Option<SandboxPermissions>,
    /// Suggests a command prefix to persist for future sessions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix_rule: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub additional_permissions: Option<PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}

/// If the `name` of a `ResponseItem::FunctionCall` is `shell_command`, the
/// `arguments` field should deserialize to this struct.
#[derive(Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
pub struct ShellCommandToolCallParams {
    pub command: String,
    pub workdir: Option<String>,

    /// Whether to run the shell with login shell semantics
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<bool>,
    /// This is the maximum time in milliseconds that the command is allowed to run.
    #[serde(alias = "timeout")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_permissions: Option<SandboxPermissions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prefix_rule: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub additional_permissions: Option<PermissionProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub justification: Option<String>,
}
