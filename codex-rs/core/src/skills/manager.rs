use std::path::Path;
use std::path::PathBuf;

use tracing::info;

use crate::config::Config;
use crate::config::types::SkillsConfig;
use crate::skills::SkillLoadOutcome;
use crate::skills::loader::SkillRoot;

pub struct SkillsManager;

impl SkillsManager {
    pub fn new(
        _codex_home: PathBuf,
        _plugins_manager: std::sync::Arc<crate::plugins::PluginsManager>,
        _bundled_skills_enabled: bool,
    ) -> Self {
        Self
    }

    pub fn skills_for_config(&self, _config: &Config) -> SkillLoadOutcome {
        SkillLoadOutcome::default()
    }

    pub(crate) fn skill_roots_for_config(&self, _config: &Config) -> Vec<SkillRoot> {
        Vec::new()
    }

    pub async fn skills_for_cwd(&self, _cwd: &Path, _force_reload: bool) -> SkillLoadOutcome {
        SkillLoadOutcome::default()
    }

    pub async fn skills_for_cwd_with_extra_user_roots(
        &self,
        _cwd: &Path,
        _force_reload: bool,
        _extra_user_roots: &[PathBuf],
    ) -> SkillLoadOutcome {
        SkillLoadOutcome::default()
    }

    pub fn clear_cache(&self) {
        info!("skills cache cleared (0 entries)");
    }
}

pub(crate) fn bundled_skills_enabled_from_stack(
    config_layer_stack: &crate::config_loader::ConfigLayerStack,
) -> bool {
    let effective_config = config_layer_stack.effective_config();
    let Some(skills_value) = effective_config
        .as_table()
        .and_then(|table| table.get("skills"))
    else {
        return true;
    };

    let skills: SkillsConfig = match skills_value.clone().try_into() {
        Ok(skills) => skills,
        Err(err) => {
            tracing::warn!("invalid skills config: {err}");
            return true;
        }
    };

    skills.bundled.unwrap_or_default().enabled
}
