use anyhow::Result;
use std::path::PathBuf;

use crate::auth::CodexAuth;
use crate::config::Config;
use chaos_ipc::protocol::RemoteSkillHazelnutScope;
use chaos_ipc::protocol::RemoteSkillProductSurface;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSkillDownloadResult {
    pub id: String,
    pub path: PathBuf,
}

pub async fn list_remote_skills(
    _config: &Config,
    _auth: Option<&CodexAuth>,
    _hazelnut_scope: RemoteSkillHazelnutScope,
    _product_surface: RemoteSkillProductSurface,
    _enabled: Option<bool>,
) -> Result<Vec<RemoteSkillSummary>> {
    Ok(Vec::new())
}

pub async fn export_remote_skill(
    _config: &Config,
    _auth: Option<&CodexAuth>,
    _hazelnut_id: &str,
) -> Result<RemoteSkillDownloadResult> {
    anyhow::bail!("remote skill export is not available (skills system stubbed)")
}
