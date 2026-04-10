use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

use crate::custom_prompts::CustomPrompt;

/// Response payload for `Op::ListCustomPrompts`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ListCustomPromptsResponseEvent {
    pub custom_prompts: Vec<CustomPrompt>,
}

/// Response payload for `Op::ListSkills`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ListSkillsResponseEvent {
    pub skills: Vec<SkillsListEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct RemoteSkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "kebab-case")]
#[ts(rename_all = "kebab-case")]
pub enum RemoteSkillHazelnutScope {
    WorkspaceShared,
    AllShared,
    Personal,
    Example,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[ts(rename_all = "lowercase")]
pub enum RemoteSkillProductSurface {
    Chatgpt,
    Chaos,
    Api,
    Atlas,
}

/// Response payload for `Op::ListRemoteSkills`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct ListRemoteSkillsResponseEvent {
    pub skills: Vec<RemoteSkillSummary>,
}

/// Response payload for `Op::DownloadRemoteSkill`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct RemoteSkillDownloadedEvent {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum SkillScope {
    User,
    Repo,
    System,
    Admin,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Legacy short_description from SKILL.md. Prefer SKILL.json interface.short_description.
    pub short_description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub interface: Option<SkillInterface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dependencies: Option<SkillDependencies>,
    pub path: PathBuf,
    pub scope: SkillScope,
    pub enabled: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillInterface {
    #[ts(optional)]
    pub display_name: Option<String>,
    #[ts(optional)]
    pub short_description: Option<String>,
    #[ts(optional)]
    pub icon_small: Option<PathBuf>,
    #[ts(optional)]
    pub icon_large: Option<PathBuf>,
    #[ts(optional)]
    pub brand_color: Option<String>,
    #[ts(optional)]
    pub default_prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillDependencies {
    pub tools: Vec<SkillToolDependency>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS, PartialEq, Eq)]
pub struct SkillToolDependency {
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub r#type: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct SkillErrorInfo {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, TS)]
pub struct SkillsListEntry {
    pub cwd: PathBuf,
    pub skills: Vec<SkillMetadata>,
    pub errors: Vec<SkillErrorInfo>,
}
