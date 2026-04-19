use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use strum_macros::Display;
use ts_rs::TS;

use super::VfsAccessMode;
use super::VfsPath;
use super::VfsSpecialPath;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct VfsEntry {
    pub path: VfsPath,
    pub access: VfsAccessMode,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, Default, JsonSchema, TS,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum VfsPolicyKind {
    #[default]
    Restricted,
    Unrestricted,
    ExternalSandbox,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct VfsPolicy {
    pub kind: VfsPolicyKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<VfsEntry>,
}

impl Default for VfsPolicy {
    fn default() -> Self {
        Self {
            kind: VfsPolicyKind::Restricted,
            entries: vec![VfsEntry {
                path: VfsPath::Special {
                    value: VfsSpecialPath::Root,
                },
                access: VfsAccessMode::Read,
            }],
        }
    }
}

impl VfsPolicy {
    pub fn unrestricted() -> Self {
        Self {
            kind: VfsPolicyKind::Unrestricted,
            entries: Vec::new(),
        }
    }

    pub fn external_sandbox() -> Self {
        Self {
            kind: VfsPolicyKind::ExternalSandbox,
            entries: Vec::new(),
        }
    }

    pub fn restricted(entries: Vec<VfsEntry>) -> Self {
        Self {
            kind: VfsPolicyKind::Restricted,
            entries,
        }
    }
}
