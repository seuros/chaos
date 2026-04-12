use chaos_realpath::AbsolutePathBuf;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

/// Controls the per-command sandbox override requested by a shell-like tool call.
#[derive(
    Debug, Clone, Copy, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "snake_case")]
pub enum SandboxPermissions {
    /// Run with the turn's configured sandbox policy unchanged.
    #[default]
    UseDefault,
    /// Request to run outside the sandbox.
    RequireEscalated,
    /// Request to stay in the sandbox while widening permissions for this
    /// command only.
    WithAdditionalPermissions,
}

impl SandboxPermissions {
    /// True if SandboxPermissions requires full unsandboxed execution (i.e. RequireEscalated)
    pub fn requires_escalated_permissions(self) -> bool {
        matches!(self, SandboxPermissions::RequireEscalated)
    }

    /// True if SandboxPermissions requests any explicit per-command override
    /// beyond `UseDefault`.
    pub fn requests_sandbox_override(self) -> bool {
        !matches!(self, SandboxPermissions::UseDefault)
    }

    /// True if SandboxPermissions uses the sandboxed per-command permission
    /// widening flow.
    pub fn uses_additional_permissions(self) -> bool {
        matches!(self, SandboxPermissions::WithAdditionalPermissions)
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FileSystemPermissions {
    pub read: Option<Vec<AbsolutePathBuf>>,
    pub write: Option<Vec<AbsolutePathBuf>>,
}

impl FileSystemPermissions {
    pub fn is_empty(&self) -> bool {
        self.read.is_none() && self.write.is_none()
    }
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct NetworkPermissions {
    pub enabled: Option<bool>,
}

impl NetworkPermissions {
    pub fn is_empty(&self) -> bool {
        self.enabled.is_none()
    }
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "snake_case")]
pub enum MacOsPreferencesPermission {
    None,
    // IMPORTANT: ReadOnly needs to be the default because it's the
    // security-sensitive default and keeps cf prefs working.
    #[default]
    ReadOnly,
    ReadWrite,
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
    TS,
)]
#[serde(rename_all = "snake_case")]
pub enum MacOsContactsPermission {
    #[default]
    None,
    ReadOnly,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case", try_from = "MacOsAutomationPermissionDe")]
pub enum MacOsAutomationPermission {
    #[default]
    None,
    All,
    BundleIds(Vec<String>),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
enum MacOsAutomationPermissionDe {
    Mode(String),
    BundleIds(Vec<String>),
    BundleIdsObject { bundle_ids: Vec<String> },
}

impl TryFrom<MacOsAutomationPermissionDe> for MacOsAutomationPermission {
    type Error = String;

    /// Accepts one of:
    /// - `"none"` or `"all"`
    /// - a plain list of bundle IDs, e.g. `["com.apple.Notes"]`
    /// - an object with bundle IDs, e.g. `{"bundle_ids": ["com.apple.Notes"]}`
    fn try_from(value: MacOsAutomationPermissionDe) -> Result<Self, Self::Error> {
        let permission = match value {
            MacOsAutomationPermissionDe::Mode(value) => {
                let normalized = value.trim().to_ascii_lowercase();
                if normalized == "all" {
                    MacOsAutomationPermission::All
                } else if normalized == "none" {
                    MacOsAutomationPermission::None
                } else {
                    return Err(format!(
                        "invalid macOS automation permission: {value}; expected none, all, or bundle ids"
                    ));
                }
            }
            MacOsAutomationPermissionDe::BundleIds(bundle_ids)
            | MacOsAutomationPermissionDe::BundleIdsObject { bundle_ids } => {
                let bundle_ids = bundle_ids
                    .into_iter()
                    .map(|bundle_id| bundle_id.trim().to_string())
                    .filter(|bundle_id| !bundle_id.is_empty())
                    .collect::<Vec<String>>();
                if bundle_ids.is_empty() {
                    MacOsAutomationPermission::None
                } else {
                    MacOsAutomationPermission::BundleIds(bundle_ids)
                }
            }
        };

        Ok(permission)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Hash, Serialize, Deserialize, JsonSchema, TS)]
#[serde(default)]
pub struct MacOsSeatbeltProfileExtensions {
    #[serde(alias = "preferences")]
    pub macos_preferences: MacOsPreferencesPermission,
    #[serde(alias = "automations")]
    pub macos_automation: MacOsAutomationPermission,
    #[serde(alias = "launch_services")]
    pub macos_launch_services: bool,
    #[serde(alias = "accessibility")]
    pub macos_accessibility: bool,
    #[serde(alias = "calendar")]
    pub macos_calendar: bool,
    #[serde(alias = "reminders")]
    pub macos_reminders: bool,
    #[serde(alias = "contacts")]
    pub macos_contacts: MacOsContactsPermission,
}

#[derive(Debug, Clone, Default, Eq, Hash, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct PermissionProfile {
    pub network: Option<NetworkPermissions>,
    pub file_system: Option<FileSystemPermissions>,
    pub macos: Option<MacOsSeatbeltProfileExtensions>,
}

impl PermissionProfile {
    pub fn is_empty(&self) -> bool {
        self.network.is_none() && self.file_system.is_none() && self.macos.is_none()
    }
}
